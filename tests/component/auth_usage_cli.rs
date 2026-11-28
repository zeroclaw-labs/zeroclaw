use std::process::Command;

#[test]
fn missing_codex_profile_keeps_stdout_empty_and_does_not_initialize_otp() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"schema_version = 4
locale = "en"

[providers.models.openai.coding]
model = "gpt-5.4"
wire_api = "responses"
requires_openai_auth = true

[risk_profiles.default]

[runtime_profiles.default]

[agents.default]
model_provider = "openai.coding"
risk_profile = "default"
runtime_profile = "default"

[security.otp]
enabled = true
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", dir.path())
        .env_remove("RUST_LOG")
        .args([
            "auth",
            "usage",
            "--model-provider",
            "openai.coding",
            "--json",
        ])
        .output()
        .expect("run zeroclaw auth usage");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "usage error polluted stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No OpenAI Codex OAuth profile found"),
        "{stderr}"
    );
    assert!(!stderr.contains("otpauth://"));
    for file in [
        "otp-secret",
        ".secret_key",
        "auth-profiles.json",
        "auth-profiles.lock",
    ] {
        assert!(
            !dir.path().join(file).exists(),
            "unexpectedly created {file}"
        );
    }
}
