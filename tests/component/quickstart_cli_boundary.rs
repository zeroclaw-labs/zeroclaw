//! Process-boundary regression coverage for CLI Quickstart's terminal contract.

use std::process::Command;

#[test]
fn quickstart_binary_requires_a_terminal_without_writing_config() {
    let config_dir = tempfile::tempdir().expect("temp config directory");
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir.path())
        .env("RUST_LOG", "off")
        .args([
            "quickstart",
            "--model-provider",
            "anthropic",
            "--model",
            "claude-sonnet-4-5",
            "--api-key",
            "synthetic-token",
            "--agent",
            "boundary-test",
        ])
        .output()
        .expect("run shipped zeroclaw quickstart binary");

    assert!(
        !output.status.success(),
        "headless Quickstart must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Quickstart is interactive and needs a terminal"));
    let config = std::fs::read_to_string(config_dir.path().join("config.toml"))
        .expect("outer CLI initializes its standard config before dispatch");
    assert!(
        !config.contains("boundary-test")
            && !config.contains("synthetic-token")
            && !config.contains("[providers.models.anthropic.default]")
    );
}
