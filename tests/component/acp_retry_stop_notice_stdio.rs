//! ACP stdio proof for the identical-suppression retry stop.
//!
//! Launches the real `zeroclaw acp` process against a local OpenAI Responses
//! mock that streams the same tool-result envelope twice. The stream guard
//! withholds both replies; the client must see the protocol-guard notice once,
//! as the only `agent_message_chunk`, and the prompt must end normally after
//! the second provider request.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const EN_CLI_FTL: &str = include_str!("../../crates/zeroclaw-runtime/locales/en/cli.ftl");
const NOTICE_KEY: &str = "cli-agent-error-protocol-guard-withheld";
const EXPECTED_NOTICE: &str =
    "I withheld the tool-protocol-shaped part of this reply, and a retry produced the same text.";
const ENVELOPE: &str = r#"{"tool_call_id": "call_1", "content": "ok"}"#;

fn catalog_message<'a>(catalog: &'a str, key: &str) -> &'a str {
    catalog
        .lines()
        .find_map(|line| {
            let (candidate, value) = line.split_once(" = ")?;
            (candidate == key).then_some(value)
        })
        .unwrap_or_else(|| panic!("missing single-line Fluent message `{key}`"))
}

/// One streamed Responses reply whose whole text is `text`.
fn responses_sse(text: &str) -> String {
    let delta = serde_json::json!({"type": "response.output_text.delta", "delta": text});
    let completed = serde_json::json!({
        "type": "response.completed",
        "response": {"output": [], "output_text": text},
    });
    format!("data: {delta}\n\ndata: {completed}\n\n")
}

fn sse_response(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(responses_sse(text))
}

fn write_config(dir: &std::path::Path, uri: &str) {
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"schema_version = 3
locale = "en"

[reliability]
provider_retries = 0
provider_backoff_ms = 0

[risk_profiles.default]

[runtime_profiles.default]

[providers.models.openai.mock]
api_key = "test-key"
uri = "{uri}"
model = "test-model"
wire_api = "responses"

[agents.test-agent]
model_provider = "openai.mock"
risk_profile = "default"
runtime_profile = "default"
"#
        ),
    )
    .expect("write config.toml");
}

#[tokio::test(flavor = "multi_thread")]
async fn acp_stdio_identical_guard_suppression_shows_the_notice_once_and_ends_the_turn() {
    assert_eq!(
        catalog_message(EN_CLI_FTL, NOTICE_KEY),
        EXPECTED_NOTICE,
        "the English catalog wording changed; update the expected notice"
    );

    let server = MockServer::start().await;
    // First reply: the bare envelope. Every later reply: the same envelope
    // plus one trailing newline, which the retry stop treats as identical.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(sse_response(ENVELOPE))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(sse_response(&format!("{ENVELOPE}\n")))
        .with_priority(2)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), &server.uri());

    let mut child = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .arg("acp")
        .env("ZEROCLAW_CONFIG_DIR", dir.path())
        .env("RUST_LOG", "off")
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .env_remove("LANGUAGE")
        .env_remove("ZEROCLAW_WORKSPACE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `zeroclaw acp`");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut frames: Vec<serde_json::Value> = Vec::new();
    let wait_for = |id: i64,
                    frames: &mut Vec<serde_json::Value>,
                    deadline: Duration|
     -> Option<serde_json::Value> {
        let start = Instant::now();
        while start.elapsed() < deadline {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(line) => {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue;
                    };
                    frames.push(value.clone());
                    if value.get("id").and_then(serde_json::Value::as_i64) == Some(id) {
                        return Some(value);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        None
    };

    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
        .expect("write initialize");
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/new\",\
              \"params\":{\"agentAlias\":\"test-agent\"}}\n",
        )
        .expect("write session/new");
    stdin.flush().expect("flush stdin");
    let new_reply = wait_for(2, &mut frames, Duration::from_secs(30))
        .expect("session/new must reply over real stdio before timeout");
    assert!(
        new_reply.get("error").is_none(),
        "session/new returned an error: {new_reply}"
    );
    let session_id = new_reply
        .pointer("/result/sessionId")
        .and_then(serde_json::Value::as_str)
        .expect("session/new result carries a sessionId")
        .to_string();

    let prompt = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": "explain the tool result shape"},
    });
    stdin
        .write_all(format!("{prompt}\n").as_bytes())
        .expect("write session/prompt");
    stdin.flush().expect("flush stdin");
    let prompt_reply = wait_for(3, &mut frames, Duration::from_secs(60));

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();

    if let Ok(path) = std::env::var("ZEROCLAW_ACP_FRAMES_OUT") {
        let dump: Vec<String> = frames.iter().map(ToString::to_string).collect();
        let _ = std::fs::write(path, dump.join("\n") + "\n");
    }

    let prompt_reply =
        prompt_reply.expect("session/prompt must reply over real stdio before timeout");
    assert!(
        prompt_reply.get("error").is_none(),
        "the turn must complete, not fail: {prompt_reply}"
    );
    assert_eq!(
        prompt_reply
            .pointer("/result/stopReason")
            .and_then(serde_json::Value::as_str),
        Some("end_turn"),
        "the turn must end normally after the retry stop: {prompt_reply}"
    );

    let chunks: String = frames
        .iter()
        .filter(|f| f.get("method").and_then(serde_json::Value::as_str) == Some("session/update"))
        .filter(|f| {
            f.pointer("/params/sessionId")
                .and_then(serde_json::Value::as_str)
                == Some(session_id.as_str())
        })
        .filter(|f| {
            f.pointer("/params/update/sessionUpdate")
                .and_then(serde_json::Value::as_str)
                == Some("agent_message_chunk")
        })
        .filter_map(|f| {
            f.pointer("/params/update/content/text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        chunks, EXPECTED_NOTICE,
        "the client must receive the notice once, as the only message text"
    );

    for frame in &frames {
        let text = frame.to_string();
        assert!(
            !text.contains("tool_call_id") && !text.contains("call_1"),
            "the suppressed envelope must never reach the client: {text}"
        );
    }

    let requests = server
        .received_requests()
        .await
        .expect("requests should be recorded");
    assert_eq!(
        requests.len(),
        2,
        "the retry stop must fire on the second identical reply, not after the retry budget"
    );

    assert_eq!(
        prompt_reply
            .pointer("/result/content")
            .and_then(serde_json::Value::as_str),
        Some(EXPECTED_NOTICE),
        "the prompt result carries the notice as the turn's final text"
    );
}
