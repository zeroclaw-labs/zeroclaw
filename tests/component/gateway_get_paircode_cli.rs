//! Binary-spawn coverage for `zeroclaw gateway get-paircode` fetch failures.
//!
//! The human-readable path used to print guidance and still return Ok(()), so
//! scripts could not tell that no code was obtained. These tests spawn the real
//! binary against an isolated config dir and assert exit status 1, the existing
//! guidance, and an empty stderr (no duplicate anyhow error reprint). One case
//! has no admin token, so the fetch fails before connecting; the other seeds a
//! dummy admin token so the fetch reaches a closed loopback port.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};

/// Pick a free loopback port, then drop the listener so nothing accepts.
fn closed_loopback_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

fn seed_config(dir: &Path) {
    std::fs::write(
        dir.join("config.toml"),
        "schema_version = 3\nlocale = \"en\"\n",
    )
    .expect("seed config");
}

fn run_get_paircode(dir: &Path, port: u16) -> Output {
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
}

fn assert_fetch_failure(output: &Output, detail: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "get-paircode must exit 1 when the gateway fetch fails; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Failed to fetch pairing code"),
        "stdout must include the fetch-failed guidance; got:\n{stdout}"
    );
    assert!(
        stdout.contains(detail),
        "stdout must include the fetch error {detail:?}; got:\n{stdout}"
    );
    assert!(
        stdout.contains("zeroclaw gateway start"),
        "stdout must include the start-gateway hint; got:\n{stdout}"
    );
    // Exiting via process::exit(1) after the human-readable guidance must not
    // also reprint the error through anyhow (`Error: ...`) on stderr.
    assert!(
        stderr.trim().is_empty(),
        "stderr must stay empty with RUST_LOG=off; stderr:\n{stderr}"
    );
}

#[test]
fn get_paircode_exits_nonzero_without_admin_token() {
    let config_dir = tempfile::tempdir().expect("temporary config directory");
    seed_config(config_dir.path());

    let output = run_get_paircode(config_dir.path(), closed_loopback_port());
    assert_fetch_failure(&output, "No gateway admin token");
}

#[test]
fn get_paircode_exits_nonzero_when_gateway_unreachable() {
    let config_dir = tempfile::tempdir().expect("temporary config directory");
    seed_config(config_dir.path());
    let data_dir = config_dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    std::fs::write(data_dir.join("gateway-admin.token"), "dummy-admin-token")
        .expect("seed dummy admin token");

    let output = run_get_paircode(config_dir.path(), closed_loopback_port());
    assert_fetch_failure(&output, "Failed to connect to gateway");
}
