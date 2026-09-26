//! Binary-spawn coverage for `zeroclaw relay claim`.
//!
//! The eight in-crate tests exercise `handle_claim` and its helpers directly.
//! None of them go through the shipped binary: clap dispatch, the global config
//! load, the default-vs-`--config-dir` data dir, the persisted config write, and
//! the process exit status are only exercised end-to-end here. This test spawns
//! the real `zeroclaw relay claim …` against an isolated config dir and a mock
//! control endpoint, then asserts the process exit status and the config write.

use std::process::Command;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A successful claim: the CLI reaches the control plane exactly once, exits 0,
/// and persists the claimed `[relay]` block into the config dir it was pointed at.
#[tokio::test]
async fn relay_claim_binary_writes_config_on_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/claim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "node_id": "node-from-cli",
            "relay_addr": "relay.cli:8443",
        })))
        .mount(&server)
        .await;

    let config_dir = tempfile::tempdir().expect("temporary config directory");
    std::fs::write(
        config_dir.path().join("config.toml"),
        "schema_version = 3\n",
    )
    .expect("seed config");

    // `server.uri()` is http://127.0.0.1:<port> — a loopback host, so the
    // cleartext guard admits it. Run the blocking child on a helper thread so it
    // cannot stall the wiremock server's runtime.
    let control = server.uri();
    let dir = config_dir.path().to_path_buf();
    let output = std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .env("RUST_LOG", "off")
            .args([
                "--config-dir",
                dir.to_str().expect("UTF-8 config path"),
                "relay",
                "claim",
                "clm-cli-token",
                "--control",
                &control,
            ])
            .output()
            .expect("run zeroclaw relay claim")
    })
    .join()
    .expect("zeroclaw relay claim process must not panic");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "`zeroclaw relay claim` must exit 0 on success; stderr:\n{stderr}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    // The CLI reached the control plane exactly once (clap dispatched the claim).
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request recording enabled")
            .len(),
        1,
        "the CLI must POST the claim to the control plane exactly once"
    );

    // The claimed relay was persisted into the config dir the CLI was pointed at.
    let written = std::fs::read_to_string(config_dir.path().join("config.toml"))
        .expect("config.toml is readable after the claim");
    assert!(written.contains("enabled = true"), "got:\n{written}");
    assert!(
        written.contains("url = \"relay.cli:8443\""),
        "got:\n{written}"
    );
    assert!(
        written.contains("node_id = \"node-from-cli\""),
        "got:\n{written}"
    );
}

/// A non-loopback cleartext `--control` is refused by the shipped binary before
/// any token leaves the process: non-zero exit, and the config is untouched.
#[tokio::test]
async fn relay_claim_binary_refuses_cleartext_non_loopback_control() {
    let config_dir = tempfile::tempdir().expect("temporary config directory");
    std::fs::write(
        config_dir.path().join("config.toml"),
        "schema_version = 3\n",
    )
    .expect("seed config");
    let before = std::fs::read_to_string(config_dir.path().join("config.toml")).unwrap();

    let dir = config_dir.path().to_path_buf();
    let output = std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .env("RUST_LOG", "off")
            .args([
                "--config-dir",
                dir.to_str().expect("UTF-8 config path"),
                "relay",
                "claim",
                "clm-cli-token",
                "--control",
                "http://control.zerorelay.net",
            ])
            .output()
            .expect("run zeroclaw relay claim")
    })
    .join()
    .expect("zeroclaw relay claim process must not panic");

    assert!(
        !output.status.success(),
        "a cleartext non-loopback --control must fail; stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let after = std::fs::read_to_string(config_dir.path().join("config.toml")).unwrap();
    assert_eq!(before, after, "a refused claim must not touch the config");
}
