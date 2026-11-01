//! Binary-spawn coverage for `zeroclaw gateway get-paircode` fetch failures.
//!
//! The human-readable path used to print guidance and still return Ok(()), so
//! scripts could not tell that no code was obtained. This spawns the real binary
//! against an isolated config dir and a closed loopback port, then asserts exit
//! status 1, the existing guidance, and no duplicate anyhow error reprint.

use std::net::TcpListener;
use std::process::Command;

/// Pick a free loopback port, then drop the listener so nothing accepts.
fn closed_loopback_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

#[test]
fn get_paircode_exits_nonzero_when_gateway_fetch_fails() {
    let config_dir = tempfile::tempdir().expect("temporary config directory");
    std::fs::write(
        config_dir.path().join("config.toml"),
        "schema_version = 3\n",
    )
    .expect("seed config");

    let port = closed_loopback_port();
    let dir = config_dir.path().to_path_buf();
    let output = std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .env("RUST_LOG", "off")
            .args([
                "--config-dir",
                dir.to_str().expect("UTF-8 config path"),
                "gateway",
                "get-paircode",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .output()
            .expect("run zeroclaw gateway get-paircode")
    })
    .join()
    .expect("get-paircode process must not panic");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "get-paircode must exit 1 when the gateway fetch fails; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Failed to fetch pairing code")
            || stdout.contains("cli-pairing-fetch-failed")
            || stdout.to_lowercase().contains("failed to fetch"),
        "stdout must include the fetch-failed guidance; got:\n{stdout}"
    );
    assert!(
        stdout.contains("gateway start") || stdout.contains("Is the gateway running"),
        "stdout must include the start-gateway hint; got:\n{stdout}"
    );
    // Exiting via process::exit(1) after the human-readable guidance should not
    // also reprint the same error through anyhow on stderr.
    assert!(
        !stderr.contains("Failed to fetch pairing code"),
        "stderr must not duplicate the human-readable fetch failure; stderr:\n{stderr}"
    );
}
