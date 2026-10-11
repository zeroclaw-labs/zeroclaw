//! Real binary proof for CLI admission; successful OAuth/inference is synthetic
//! through the production handler/factory in src/main.rs unit tests.
use std::process::Command;

#[test]
fn chatgpt_plan_cli_help_exposes_tools_disabled_check() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args([
            "--config-dir",
            root.path().to_str().unwrap(),
            "auth",
            "plan-check",
            "--help",
        ])
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--model-provider"));
    assert!(help.contains("--message"));
}

#[cfg(all(feature = "agent-runtime", unix))]
#[test]
fn chatgpt_plan_cli_unbound_check_fails_without_ambient_key_fallback() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("config.toml"), "locale = \"en\"\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args([
            "--config-dir",
            root.path().to_str().unwrap(),
            "auth",
            "plan-check",
            "--model-provider",
            "openai.missing",
            "--message",
            "fixture",
        ])
        .env("OPENAI_API_KEY", "synthetic-metered-key")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("ChatGPT plan provider alias missing"));
    assert!(!stderr.contains("synthetic-metered-key"));
    assert!(!root.path().join("auth-profiles.json").exists());
}
