//! Component coverage for `zeroclaw eval run --mode live`.
//!
//! The live-mode unit tests inject a provider directly, so they never cross the
//! user-facing path: CLI parsing, config loading, `[eval].live_provider`
//! resolution, real provider construction, one completed model turn, and the
//! graded report. This launches the production binary against a local
//! OpenAI-compatible mock so that whole path is exercised without credentials
//! and without external network.

use std::process::Command;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ECHO_CASE: &str = r#"{
  "model_name": "live-cli-echo",
  "turns": [{ "user_input": "Reply with the word hello and nothing else." }],
  "expects": { "response_contains": ["hello"], "max_tool_calls": 0 }
}"#;

#[tokio::test]
async fn eval_run_live_drives_the_configured_provider_through_the_cli() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "mock-echo",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "hello from the mock endpoint" }
            }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16 }
        })))
        .mount(&server)
        .await;

    let suite = tempfile::tempdir().expect("temporary suite directory");
    std::fs::write(suite.path().join("live_echo.json"), ECHO_CASE).expect("write live case");

    let config_dir = tempfile::tempdir().expect("temporary config directory");
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!(
            r#"schema_version = 3

[reliability]
provider_retries = 0
provider_backoff_ms = 0

[risk_profiles.default]

[runtime_profiles.default]

[providers.models.custom.mock]
api_key = "test-key"
uri = "{}"
model = "mock-echo"
wire_api = "chat_completions"

[eval]
live_provider = "custom.mock"
case_timeout_secs = 30
"#,
            server.uri()
        ),
    )
    .expect("write test config");

    let config_dir_arg = config_dir.path().to_path_buf();
    let suite_arg = suite.path().to_path_buf();
    let output = std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .env("RUST_LOG", "off")
            .args([
                "--config-dir",
                config_dir_arg.to_str().expect("UTF-8 config path"),
                "eval",
                "run",
                "--suite",
                suite_arg.to_str().expect("UTF-8 suite path"),
                "--mode",
                "live",
                "--format",
                "json",
            ])
            .output()
            .expect("run zeroclaw eval")
    })
    .join()
    .expect("zeroclaw eval process must not panic");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a passing live suite must exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("eval must print a JSON report");
    assert_eq!(
        report["all_passed"], true,
        "the live case must grade green: {report}"
    );
    assert_eq!(report["cases"][0]["name"], "live-cli-echo");

    // The request that actually left the process must carry the configured
    // model. A regression that drops the resolved metadata sends
    // `Agent::builder()`'s placeholder instead, which no injected-provider test
    // can catch because those providers ignore the model argument.
    let requests = server
        .received_requests()
        .await
        .expect("request recording enabled");
    let chat: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .collect();
    assert_eq!(
        chat.len(),
        1,
        "the live case must complete exactly one model turn"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&chat[0].body).expect("provider request must be JSON");
    assert_eq!(
        body["model"], "mock-echo",
        "the configured model must reach the provider: {body}"
    );
}
