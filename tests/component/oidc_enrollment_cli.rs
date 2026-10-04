//! `zeroclaw oidc token <alias>` through the real command entry point: stdout
//! carries exactly the access token on success and nothing on failure, with
//! the OTP prelude enabled and no seed on disk (the case where startup would
//! otherwise print a freshly minted OTP enrollment URI to stdout).

use std::process::{Command, Stdio};

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write_config(dir: &std::path::Path, issuer: &str) {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"schema_version = 3

[providers.models.ollama.default]
model = "test-model"

[risk_profiles.default]

[runtime_profiles.default]

[agents.default]
model_provider = "ollama.default"
risk_profile = "default"
runtime_profile = "default"

[security.otp]
enabled = true

[permission_profiles.service]
grants = {{ sessions = ["read"] }}

[oidc.corp]
issuer = "{issuer}"
audience = "zeroclaw"
client_id = "svc"
client_secret = "s3cr3t"
service_profile_map = {{ "svc" = "service" }}
"#
        ),
    )
    .expect("write config");
}

fn run_enrollment(config_dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(["oidc", "token", "corp"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run zeroclaw oidc token")
}

async fn idp(token_response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    let issuer = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": issuer,
            "token_endpoint": format!("{issuer}/token"),
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=client_credentials"))
        .respond_with(token_response)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn stdout_is_exactly_the_token_on_success_and_empty_on_failure() {
    let granted = idp(ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "access_token": "tok-123",
        "token_type": "Bearer",
        "expires_in": 300,
    })))
    .await;
    let config_dir = tempfile::tempdir().expect("temp config dir");
    write_config(config_dir.path(), &granted.uri());
    assert!(
        !config_dir.path().join("otp.seed").exists()
            && std::fs::read_dir(config_dir.path()).unwrap().count() == 1,
        "the OTP seed must be absent so the prelude would have minted one"
    );
    let dir = config_dir.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || run_enrollment(&dir))
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout} stderr={stderr}");
    assert_eq!(
        stdout, "tok-123\n",
        "stdout must be exactly the token; stderr was: {stderr}"
    );
    assert!(
        !stdout.contains("otpauth") && !stdout.contains("Enrollment URI"),
        "no OTP disclosure may reach stdout"
    );

    let refused = idp(ResponseTemplate::new(400).set_body_json(serde_json::json!({
        "error": "invalid_client",
        "error_description": "the secret is wrong",
    })))
    .await;
    let config_dir = tempfile::tempdir().expect("temp config dir");
    write_config(config_dir.path(), &refused.uri());
    let dir = config_dir.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || run_enrollment(&dir))
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stdout={stdout} stderr={stderr}");
    assert_eq!(
        stdout, "",
        "a failed enrollment leaves stdout empty; stderr was: {stderr}"
    );
    assert!(stderr.contains("invalid_client"), "{stderr}");
}
