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
async fn interactive_agent_keeps_literal_media_text_without_loading_images() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            let attempt = attempts.fetch_add(1, Ordering::Relaxed);
            let body: serde_json::Value =
                serde_json::from_slice(&request.body).expect("provider request JSON");
            let has_image = body["messages"]
                .as_array()
                .expect("chat messages")
                .iter()
                .any(|message| {
                    message["content"]
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))
                });
            if has_image {
                return ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": {"message": "image-bearing request rejected"}
                }));
            }
            let text = if attempt == 0 {
                "Literal media text preserved."
            } else {
                "Next text-only turn succeeded."
            };
            if body["stream"] == true {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        serde_json::json!({
                            "choices": [{"delta": {"content": text}, "finish_reason": "stop"}]
                        })
                    ))
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{"message": {"content": text}}]
                }))
            }
        })
        .mount(&server)
        .await;

    let config_dir = tempfile::tempdir().expect("temporary config directory");
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!(
            r#"schema_version = 3
locale = "en"

[reliability]
provider_retries = 0
provider_backoff_ms = 0

[risk_profiles.default]

[runtime_profiles.default]

[providers.models.custom.mock]
api_key = "test-key"
uri = "{}"
model = "test-model"
vision = true
native_tools = false
wire_api = "chat_completions"

[agents.default]
model_provider = "custom.mock"
risk_profile = "default"
runtime_profile = "default"
"#,
            server.uri()
        ),
    )
    .expect("write test config");
    let session_file = config_dir.path().join("session.json");
    let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
    let image_marker = format!("[IMAGE:{image}]");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("RUST_LOG", "off")
        .current_dir(config_dir.path())
        .arg("--config-dir")
        .arg(config_dir.path())
        .args(["agent", "--agent", "default", "--session-state-file"])
        .arg(&session_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("start interactive zeroclaw agent");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(format!("inspect {image_marker}\nnext text-only turn\n/quit\n").as_bytes())
        .await
        .expect("send two interactive turns and quit");
    let output = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
        .await
        .expect("interactive CLI must finish within 90 seconds")
        .expect("wait for interactive zeroclaw agent");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "interactive CLI must recover and exit successfully\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Literal media text preserved."));
    assert!(stdout.contains("Next text-only turn succeeded."));
    assert!(!stdout.contains("was omitted after the provider rejected"));

    let requests = server.received_requests().await.expect("recorded requests");
    let bodies: Vec<serde_json::Value> = requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("provider request JSON"))
        .collect();
    let image_counts: Vec<usize> = bodies
        .iter()
        .map(|body| {
            body["messages"]
                .as_array()
                .expect("chat messages")
                .iter()
                .filter_map(|message| message["content"].as_array())
                .flatten()
                .filter(|part| part["type"] == "image_url")
                .count()
        })
        .collect();
    assert_eq!(image_counts, [0, 0]);
    assert_eq!(
        bodies
            .iter()
            .map(|body| body["stream"] == true)
            .collect::<Vec<_>>(),
        [true, true],
        "both literal text turns stream normally"
    );
    assert!(bodies.iter().all(|body| body["model"] == "test-model"));
    assert!(
        bodies[1]["messages"]
            .as_array()
            .expect("next-turn messages")
            .last()
            .expect("newest user message")["content"]
            .as_str()
            .is_some_and(|text| text.ends_with("next text-only turn")),
        "the second request must be the next user turn"
    );
    let lease = zeroclaw_infra::session_events::cli_state::CliStateGuard::acquire(&session_file)
        .expect("native CLI binding");
    let binding = lease.binding().expect("published native family").clone();
    drop(lease);
    let mut config = zeroclaw_config::schema::Config::default();
    config.agents.insert(
        "default".into(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
    );
    let authority = zeroclaw_runtime::LiveConfigAuthority::new(config);
    let session = zeroclaw::session_events::open_cli_session(
        &binding.data_dir,
        "sqlite",
        "default",
        authority.execution_capability(),
        Some(&session_file),
    )
    .await
    .expect("reopen canonical CLI history");
    let saved = session
        .restore_context()
        .await
        .expect("restore history")
        .expect("canonical context");
    assert!(
        saved
            .chat_projection()
            .iter()
            .any(|message| { message.role == "user" && message.content.contains(&image_marker) }),
        "canonical history must preserve literal marker text"
    );
    eprintln!("CLI evidence (locale=en; request image counts={image_counts:?}):\n{stdout}");
}

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
