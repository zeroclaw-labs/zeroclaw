//! Regression coverage for direct `zeroclaw agent` terminal-failure delivery.
//!
//! This launches the production binary against a local OpenAI-compatible mock.
//! It proves the single-shot CLI boundary renders the Fluent message instead of
//! returning the stable provider diagnostic after Reliable exhausts an empty
//! completion.

use std::io::Write;
use std::process::{Command, Stdio};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn single_shot_agent_localizes_semantic_empty_terminal_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "   "}}]
        })))
        .mount(&server)
        .await;

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

[providers.models.openai.mock]
api_key = "test-key"
uri = "{}"
model = "test-model"
wire_api = "chat_completions"

[agents.default]
model_provider = "openai.mock"
risk_profile = "default"
runtime_profile = "default"
"#,
            server.uri()
        ),
    )
    .expect("write test config");

    let config_dir_arg = config_dir.path().to_path_buf();
    let output = std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .env("RUST_LOG", "off")
            .args([
                "--config-dir",
                config_dir_arg.to_str().expect("UTF-8 config path"),
                "agent",
                "--agent",
                "default",
                "--message",
                "test prompt",
            ])
            .output()
            .expect("run zeroclaw agent")
    })
    .join()
    .expect("zeroclaw agent process must not panic");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = zeroclaw_runtime::agent::semantic_empty_terminal_completion_message(None);
    assert!(
        !output.status.success(),
        "semantic-empty terminal completion must fail\nstdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains(&expected),
        "direct CLI must render the Fluent terminal-failure message `{expected}`; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("provider completed without final text or tool calls"),
        "direct CLI must not expose the stable diagnostic; stderr:\n{stderr}"
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request recording enabled")
            .len(),
        1,
        "the CLI must reach the local provider fixture exactly once"
    );
}

#[tokio::test]
async fn interactive_agent_localizes_semantic_empty_terminal_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "   "}}]
        })))
        .mount(&server)
        .await;

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

[providers.models.openai.mock]
api_key = "test-key"
uri = "{}"
model = "test-model"
wire_api = "chat_completions"

[agents.default]
model_provider = "openai.mock"
risk_profile = "default"
runtime_profile = "default"
"#,
            server.uri()
        ),
    )
    .expect("write test config");

    let config_dir_arg = config_dir.path().to_path_buf();
    let output = std::thread::spawn(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .env("RUST_LOG", "off")
            .args([
                "--config-dir",
                config_dir_arg.to_str().expect("UTF-8 config path"),
                "agent",
                "--agent",
                "default",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start interactive zeroclaw agent");
        child
            .stdin
            .as_mut()
            .expect("piped stdin")
            .write_all(b"test prompt\n/quit\n")
            .expect("send interactive prompt and quit");
        child
            .wait_with_output()
            .expect("wait for interactive zeroclaw agent")
    })
    .join()
    .expect("interactive zeroclaw agent process must not panic");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = zeroclaw_runtime::agent::semantic_empty_terminal_completion_message(None);
    assert!(
        output.status.success(),
        "interactive CLI handles the terminal error then exits on /quit\nstdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains(&expected),
        "interactive CLI must render the Fluent terminal-failure message `{expected}`; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("provider completed without final text or tool calls"),
        "interactive CLI must not expose the stable diagnostic; stderr:\n{stderr}"
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("request recording enabled")
            .len(),
        1,
        "the interactive CLI must reach the local provider fixture exactly once"
    );
}
