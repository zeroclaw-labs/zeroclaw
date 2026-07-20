use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_config::schema::{HailoOllamaModelProviderConfig, ModelProviderConfig};
use zeroclaw_providers::ModelProviderRuntimeOptions;
use zeroclaw_providers::factory::FamilyProviderFactory;
use zeroclaw_providers::hailo_ollama::HailoOllamaModelProvider;
use zeroclaw_providers::ollama::{OllamaModelProvider, OllamaTuning};
use zeroclaw_providers::traits::{ChatMessage, ModelProvider};

type Capture = Arc<Mutex<Option<Value>>>;
type RawCapture = Arc<Mutex<Option<Vec<u8>>>>;
type HeaderCapture = Arc<Mutex<Option<HeaderMap>>>;

fn hailo_provider(base_url: &str) -> HailoOllamaModelProvider {
    hailo_provider_with_queue_timeout(base_url, 5)
}

fn hailo_provider_with_queue_timeout(
    base_url: &str,
    queue_timeout_secs: u64,
) -> HailoOllamaModelProvider {
    HailoOllamaModelProvider::new(
        "edge",
        Some(base_url),
        5,
        queue_timeout_secs,
        OllamaTuning {
            num_ctx: 2048,
            num_predict: 64,
            temperature_override: None,
        },
    )
    .expect("valid fake Hailo URL")
}

async fn capture_chat(State(capture): State<Capture>, Json(body): Json<Value>) -> Json<Value> {
    *capture.lock().expect("capture lock") = Some(body);
    Json(json!({
        "message": {"role": "assistant", "content": "HAILO_NATIVE_OK"},
        "done": true,
        "prompt_eval_count": 7,
        "eval_count": 3
    }))
}

async fn capture_raw_chat(State(capture): State<RawCapture>, body: Bytes) -> Json<Value> {
    *capture.lock().expect("raw capture lock") = Some(body.to_vec());
    Json(json!({
        "message": {"role": "assistant", "content": "HAILO_NATIVE_OK"},
        "done": true,
        "prompt_eval_count": 7,
        "eval_count": 3
    }))
}

async fn capture_chat_headers(
    State(capture): State<HeaderCapture>,
    headers: HeaderMap,
) -> Json<Value> {
    *capture.lock().expect("header capture lock") = Some(headers);
    Json(json!({
        "message": {"role": "assistant", "content": "HAILO_NATIVE_OK"},
        "done": true,
        "prompt_eval_count": 7,
        "eval_count": 3
    }))
}

#[tokio::test]
async fn native_hailo_chat_requests_connection_close() {
    let capture: HeaderCapture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat_headers))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect("native Hailo request succeeds");

    let headers = capture
        .lock()
        .expect("header capture lock")
        .clone()
        .expect("request headers captured");
    assert_eq!(
        headers
            .get(axum::http::header::CONNECTION)
            .and_then(|value| value.to_str().ok()),
        Some("close")
    );

    server.abort();
}

#[tokio::test]
async fn standard_ollama_chat_does_not_force_connection_close() {
    let capture: HeaderCapture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Ollama server");
    let addr = listener.local_addr().expect("fake Ollama address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat_headers))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Ollama server");
    });

    let provider = OllamaModelProvider::new("standard", Some(&format!("http://{addr}")), None);
    provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect("standard Ollama request succeeds");

    let headers = capture
        .lock()
        .expect("header capture lock")
        .clone()
        .expect("request headers captured");
    assert!(headers.get(axum::http::header::CONNECTION).is_none());

    server.abort();
}

#[tokio::test]
async fn native_hailo_normalizes_messages_and_reports_honest_capabilities() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}/api/chat"));
    let reply = provider
        .chat_with_history(
            &[
                ChatMessage::system("Keep\nformat\tone line."),
                ChatMessage::user("First line\r\nSecond line.\0"),
            ],
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect("native Hailo chat succeeds");

    assert_eq!(reply, "HAILO_NATIVE_OK");
    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    assert_eq!(body["model"], "qwen3:1.7b");
    assert_eq!(body["stream"], false);
    assert_eq!(body["think"], false);
    assert_eq!(body["options"]["num_ctx"], 2048);
    assert_eq!(body["options"]["num_predict"], 64);
    assert_eq!(body["options"]["temperature"], 0.2);
    assert_eq!(body["messages"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(
        body["messages"][0]["content"],
        "Instructions: Keep\\nformat\\tone line. Request: First line\\r\\nSecond line."
    );
    assert!(body.get("tools").is_none());

    let capabilities = provider.capabilities();
    assert!(!capabilities.native_tool_calling);
    assert!(!capabilities.vision);
    assert!(!provider.supports_streaming());
    assert_eq!(
        provider.role(),
        Role::Provider(ProviderKind::Model(ModelProviderKind::HailoOllama))
    );
    assert_eq!(provider.alias(), "edge");
    server.abort();
}

#[tokio::test]
async fn native_hailo_preserves_model_ids_ending_in_cloud() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    hailo_provider(&format!("http://{addr}"))
        .simple_chat("hello", "local-model:cloud", Some(0.2))
        .await
        .expect("Hailo must treat model IDs as opaque");

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    assert_eq!(body["model"], "local-model:cloud");
    server.abort();
}

#[tokio::test]
async fn native_hailo_emits_ascii_only_json_for_wire_compatibility() {
    let capture: RawCapture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_raw_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    let messages = [
        ChatMessage::system("Identity — concise."),
        ChatMessage::user("Vastaa yhdellä virkkeellä: näyttö, sähkökatkos ja testi 🧪."),
    ];
    provider
        .chat(
            zeroclaw_api::model_provider::ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect("native Hailo request succeeds");

    let raw = capture
        .lock()
        .expect("raw capture lock")
        .clone()
        .expect("raw request captured");
    let body: Value = serde_json::from_slice(&raw).expect("captured request is valid JSON");
    let content = body["messages"][0]["content"]
        .as_str()
        .expect("captured Unicode content");
    assert!(content.contains("Identity — concise."));
    assert!(content.contains("näyttö, sähkökatkos ja testi 🧪"));
    assert!(
        raw.is_ascii(),
        "native Hailo request body must contain ASCII-only JSON"
    );
    assert!(raw.windows(6).any(|window| window == br"\u2014"));
    assert!(raw.windows(6).any(|window| window == br"\u00e4"));
    assert!(raw.windows(6).any(|window| window == br"\u00f6"));
    assert!(raw.windows(12).any(|window| window == br"\ud83e\uddea"));

    server.abort();
}

#[tokio::test]
async fn standard_ollama_keeps_default_non_ascii_json_serialization() {
    let capture: RawCapture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Ollama server");
    let addr = listener.local_addr().expect("fake Ollama address");
    let app = Router::new()
        .route("/api/chat", post(capture_raw_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Ollama server");
    });

    let provider = OllamaModelProvider::new("standard", Some(&format!("http://{addr}")), None);
    let messages = [ChatMessage::user("näyttö ja testi 🧪")];
    provider
        .chat(
            zeroclaw_api::model_provider::ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect("standard Ollama request succeeds");

    let raw = capture
        .lock()
        .expect("raw capture lock")
        .clone()
        .expect("raw request captured");
    let body: Value = serde_json::from_slice(&raw).expect("captured request is valid JSON");
    assert_eq!(body["messages"][0]["content"], "näyttö ja testi 🧪");
    assert!(!raw.is_ascii());
    assert!(
        raw.windows("ä".len())
            .any(|window| window == "ä".as_bytes())
    );
    assert!(
        raw.windows("🧪".len())
            .any(|window| window == "🧪".as_bytes())
    );
    assert!(!raw.windows(6).any(|window| window == br"\u00e4"));
    assert!(!raw.windows(12).any(|window| window == br"\ud83e\uddea"));

    server.abort();
}

#[tokio::test]
async fn native_hailo_omits_temperature_when_caller_does_not_set_it() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });
    let provider = hailo_provider(&format!("http://{addr}"));

    let messages = [ChatMessage::user("hello")];
    provider
        .chat(
            zeroclaw_api::model_provider::ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "qwen3:1.7b",
            None,
        )
        .await
        .expect("native Hailo request succeeds");

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    assert!(
        body["options"].get("temperature").is_none(),
        "temperature=None must omit the wire field: {body}"
    );

    server.abort();
}

#[tokio::test]
async fn native_hailo_rejects_native_tool_payloads_before_http() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });
    let provider = hailo_provider(&format!("http://{addr}"));
    let messages = [ChatMessage::user("read a file")];
    let tools = [json!({
        "type": "function",
        "function": {
            "name": "file_read",
            "description": "Read a file",
            "parameters": {"type": "object"}
        }
    })];

    let error = provider
        .chat_with_tools(&messages, &tools, "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("native Hailo tools must be rejected")
        .to_string();

    assert!(error.contains("does not support native tool calling"));
    assert!(capture.lock().expect("capture lock").is_none());
    server.abort();
}

#[tokio::test]
async fn native_hailo_rejects_truncated_prompt_guided_tool_protocol() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });
    let provider = hailo_provider(&format!("http://{addr}"));
    let system = format!(
        "Identity: {}\n\n## Tool Use Protocol\nUse <tool_call>.\n### Available Tools\n- file_read(path)",
        "identity context ".repeat(180)
    );
    let messages = [
        ChatMessage::system(system),
        ChatMessage::user("Read /tmp/example"),
    ];

    let error = provider
        .chat(
            zeroclaw_api::model_provider::ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect_err("oversized prompt-guided tools must fail closed")
        .to_string();

    assert!(
        error.contains("prompt-guided tool instructions exceed"),
        "unexpected tool prompt error: {error}"
    );
    assert!(capture.lock().expect("capture lock").is_none());
    server.abort();
}

#[tokio::test]
async fn native_hailo_bounds_history_and_preserves_latest_user_tail() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let mut history = vec![
        ChatMessage::system(format!("{}\nSYSTEM_TAIL", "s".repeat(3_000))),
        ChatMessage::assistant("orphan assistant"),
    ];
    for index in 0..8 {
        history.push(ChatMessage::user(format!("u{index}")));
        history.push(ChatMessage::assistant(format!("a{index}")));
    }
    history.push(ChatMessage::user(format!(
        "LATEST_HEAD{}LATEST_TAIL",
        "x".repeat(3_000)
    )));

    hailo_provider(&format!("http://{addr}"))
        .chat_with_history(&history, "qwen3:1.7b", Some(0.2))
        .await
        .expect("bounded native Hailo history succeeds");

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 11);
    assert_eq!(messages[0]["role"], "user");
    assert!(
        messages[0]["content"]
            .as_str()
            .expect("first content")
            .starts_with("Instructions: ")
    );
    assert!(
        messages[0]["content"]
            .as_str()
            .expect("first content")
            .contains("Request: u3")
    );
    assert_eq!(messages.last().expect("latest message")["role"], "user");
    assert!(
        messages.last().expect("latest message")["content"]
            .as_str()
            .expect("latest content")
            .ends_with("LATEST_TAIL")
    );
    for message in messages {
        let content = message["content"].as_str().expect("message content");
        assert!(content.chars().count() <= 2_000);
        assert!(
            !content
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\r' | '\n' | '\t')),
            "only non-structural control characters should be removed"
        );
    }

    let first_content = messages[0]["content"].as_str().expect("first content");
    assert!(
        first_content.contains('\n')
            || first_content.contains('\t')
            || first_content.contains(r"\n")
            || first_content.contains(r"\t")
    );

    server.abort();
}

#[tokio::test]
async fn native_hailo_fold_reallocates_unused_system_budget_to_user() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });
    let user = format!("FULL_HEAD{}FULL_TAIL", "x".repeat(1_500));

    hailo_provider(&format!("http://{addr}"))
        .chat_with_system(Some("Short system."), &user, "qwen3:1.7b", Some(0.2))
        .await
        .expect("native Hailo fold succeeds");

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    let content = body["messages"][0]["content"]
        .as_str()
        .expect("folded content");
    assert!(content.ends_with("FULL_TAIL"));
    assert!(
        !content.contains("..."),
        "user content was truncated despite spare system budget"
    );
    assert!(content.chars().count() <= 2_000);

    server.abort();
}

#[tokio::test]
async fn native_hailo_preserves_tool_history_as_plain_messages() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let history = vec![
        ChatMessage::system("Use the available tools when needed."),
        ChatMessage::user("Read README.md"),
        ChatMessage::assistant(
            r#"{"content":null,"tool_calls":[{"id":"call_1","name":"file_read","arguments":"{\"path\":\"README.md\"}"}]}"#,
        ),
        ChatMessage::tool("file contents"),
    ];
    hailo_provider(&format!("http://{addr}"))
        .chat_with_history(&history, "qwen3:1.7b", Some(0.2))
        .await
        .expect("tool history conversion succeeds");

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
    assert!(
        messages[1]["content"]
            .as_str()
            .expect("assistant tool-call prose")
            .contains("file_read")
    );
    assert!(messages[1].get("tool_calls").is_none());
    assert_eq!(messages[2]["role"], "user");
    assert!(
        messages[2]["content"]
            .as_str()
            .expect("tool-result prose")
            .contains("file contents")
    );

    server.abort();
}

#[tokio::test]
async fn native_hailo_history_boundary_drops_orphaned_tool_exchange() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let mut history = vec![
        ChatMessage::system("Keep complete tool exchanges."),
        ChatMessage::user("old request"),
        ChatMessage::assistant(
            r#"{"content":null,"tool_calls":[{"id":"call_old","name":"file_read","arguments":"{\"path\":\"old\"}"}]}"#,
        ),
        ChatMessage::tool("orphaned old result"),
    ];
    for index in 0..5 {
        history.push(ChatMessage::user(format!("fresh user {index}")));
        history.push(ChatMessage::assistant(format!("fresh assistant {index}")));
    }

    hailo_provider(&format!("http://{addr}"))
        .chat_with_history(&history, "qwen3:1.7b", Some(0.2))
        .await
        .expect("bounded tool history succeeds");

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 10);
    let first = messages[0]["content"].as_str().expect("first content");
    assert!(first.contains("Request: fresh user 0"));
    assert!(!first.contains("orphaned old result"));
    assert!(messages.iter().all(|message| {
        !message["content"]
            .as_str()
            .unwrap_or_default()
            .contains("file_read")
    }));

    server.abort();
}

#[tokio::test]
async fn native_hailo_lists_only_models_from_api_tags() {
    async fn tags() -> Json<Value> {
        Json(json!({
            "models": [
                {"name": "qwen3:1.7b"},
                {"name": "qwen2.5-coder:1.5b"}
            ]
        }))
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, Router::new().route("/api/tags", get(tags)))
            .await
            .expect("serve fake Hailo server");
    });

    let models = hailo_provider(&format!("http://{addr}"))
        .list_models()
        .await
        .expect("native Hailo model listing succeeds");
    assert_eq!(models, vec!["qwen3:1.7b", "qwen2.5-coder:1.5b"]);

    server.abort();
}

#[tokio::test]
async fn native_hailo_preserves_non_success_status_and_error() {
    async fn missing_model() -> (StatusCode, Json<Value>) {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "model 'missing:0' not found"})),
        )
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(
            listener,
            Router::new().route("/api/chat", post(missing_model)),
        )
        .await
        .expect("serve fake Hailo server");
    });

    let error = hailo_provider(&format!("http://{addr}"))
        .simple_chat("hello", "missing:0", Some(0.2))
        .await
        .expect_err("native Hailo 404 must remain an error")
        .to_string();
    assert!(error.contains("404"), "status missing from error: {error}");
    assert!(
        error.contains("model 'missing:0' not found"),
        "bounded backend detail missing from error: {error}"
    );
    assert!(
        error.contains("Hailo-Ollama API error"),
        "explicit provider missing from error: {error}"
    );
    assert!(
        error.contains("Check that Hailo-Ollama is running and the model is loaded"),
        "Hailo recovery hint missing from error: {error}"
    );
    assert!(
        !error.contains("brew install ollama"),
        "ordinary Ollama guidance leaked into Hailo error: {error}"
    );

    server.abort();
}

#[derive(Clone)]
struct ConcurrencyState {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

async fn slow_chat(State(state): State<ConcurrencyState>) -> Json<Value> {
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.max_active.fetch_max(active, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    state.active.fetch_sub(1, Ordering::SeqCst);
    Json(json!({
        "message": {"role": "assistant", "content": "ok"},
        "done": true
    }))
}

#[tokio::test]
async fn native_hailo_generation_is_single_flight() {
    let state = ConcurrencyState {
        active: Arc::new(AtomicUsize::new(0)),
        max_active: Arc::new(AtomicUsize::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(slow_chat))
        .with_state(state.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    let (first, second) = tokio::join!(
        provider.simple_chat("one", "qwen3:1.7b", Some(0.2)),
        provider.simple_chat("two", "qwen3:1.7b", Some(0.2)),
    );
    first.expect("first Hailo request succeeds");
    second.expect("second Hailo request succeeds");
    assert_eq!(state.max_active.load(Ordering::SeqCst), 1);

    server.abort();
}

#[tokio::test]
async fn independent_hailo_providers_share_normalized_endpoint_gate() {
    let state = ConcurrencyState {
        active: Arc::new(AtomicUsize::new(0)),
        max_active: Arc::new(AtomicUsize::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(slow_chat))
        .with_state(state.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let first = hailo_provider(&format!("HTTP://LOCALHOST:{}/api/chat", addr.port()));
    let second = hailo_provider(&format!("http://127.0.0.1:{}/", addr.port()));
    let (first_result, second_result) = tokio::join!(
        first.simple_chat("one", "qwen3:1.7b", Some(0.2)),
        second.simple_chat("two", "qwen3:1.7b", Some(0.2)),
    );
    first_result.expect("first independent Hailo provider succeeds");
    second_result.expect("second independent Hailo provider succeeds");
    assert_eq!(state.max_active.load(Ordering::SeqCst), 1);

    server.abort();
}

#[tokio::test]
async fn typed_hailo_factory_applies_context_tokens_timeout_and_alias() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let config = HailoOllamaModelProviderConfig {
        base: ModelProviderConfig {
            context_window: Some(1024),
            ..Default::default()
        },
        queue_timeout_secs: Some(4),
    };
    let options = ModelProviderRuntimeOptions {
        provider_timeout_secs: Some(7),
        provider_max_tokens: Some(96),
        native_tools: Some(false),
        ..Default::default()
    };
    let provider = config
        .create_provider(
            "factory_canary",
            None,
            Some(&format!("http://{addr}")),
            &options,
        )
        .expect("typed Hailo factory succeeds");
    let response = provider
        .chat_with_system(
            Some("Factory\ncontract"),
            "Reply once",
            "qwen3:1.7b",
            Some(0.1),
        )
        .await
        .expect("factory-built native Hailo chat succeeds");
    assert_eq!(response, "HAILO_NATIVE_OK");
    assert_eq!(provider.default_timeout_secs(), 7);
    assert_eq!(provider.alias(), "factory_canary");
    assert_eq!(
        provider.role(),
        Role::Provider(ProviderKind::Model(ModelProviderKind::HailoOllama))
    );

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    assert_eq!(body["options"]["num_ctx"], 1024);
    assert_eq!(body["options"]["num_predict"], 96);

    server.abort();
}

#[test]
fn typed_hailo_factory_rejects_unsupported_auth_and_native_tools() {
    let config = HailoOllamaModelProviderConfig::default();
    let error = match config.create_provider(
        "bad_auth",
        Some("not-supported"),
        Some("http://127.0.0.1:8000"),
        &ModelProviderRuntimeOptions::default(),
    ) {
        Ok(_) => panic!("Hailo API key must be rejected"),
        Err(error) => error,
    }
    .to_string();
    assert!(error.contains("does not support API-key authentication"));

    let error = match config.create_provider(
        "bad_tools",
        None,
        Some("http://127.0.0.1:8000"),
        &ModelProviderRuntimeOptions {
            native_tools: Some(true),
            ..Default::default()
        },
    ) {
        Ok(_) => panic!("native Hailo tools must be rejected"),
        Err(error) => error,
    }
    .to_string();
    assert!(error.contains("does not support native tool calling"));
}

#[test]
fn typed_hailo_factory_rejects_unsupported_shared_options() {
    let config = HailoOllamaModelProviderConfig::default();
    let cases = [
        (
            "extra_headers",
            ModelProviderRuntimeOptions {
                extra_headers: [("X-Route".to_string(), "canary".to_string())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        ),
        (
            "tls_ca_cert_path",
            ModelProviderRuntimeOptions {
                tls_ca_cert_path: Some("/tmp/private-ca.pem".to_string()),
                ..Default::default()
            },
        ),
        (
            "think",
            ModelProviderRuntimeOptions {
                think: Some(true),
                ..Default::default()
            },
        ),
        (
            "provider_extra",
            ModelProviderRuntimeOptions {
                provider_extra: Some(json!({"seed": 7})),
                ..Default::default()
            },
        ),
        (
            "api_path",
            ModelProviderRuntimeOptions {
                api_path: Some("/custom/chat".to_string()),
                ..Default::default()
            },
        ),
        (
            "wire_api",
            ModelProviderRuntimeOptions {
                wire_api: Some("responses".to_string()),
                ..Default::default()
            },
        ),
        (
            "chat_template_kwargs",
            ModelProviderRuntimeOptions {
                chat_template_kwargs: Some(json!({"add_generation_prompt": false})),
                ..Default::default()
            },
        ),
    ];

    for (field, options) in cases {
        let error = match config.create_provider(
            "unsupported_option",
            None,
            Some("http://127.0.0.1:8000"),
            &options,
        ) {
            Ok(_) => panic!("unsupported Hailo option must be rejected"),
            Err(error) => error,
        }
        .to_string();
        assert!(error.contains(field), "{field} missing from error: {error}");
    }
}

#[tokio::test]
async fn native_hailo_rejects_image_inputs_instead_of_dropping_them() {
    let provider = hailo_provider("http://127.0.0.1:9");
    let error = provider
        .simple_chat(
            "Describe this [IMAGE:data:image/png;base64,abcd]",
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect_err("native Hailo image input must fail before HTTP")
        .to_string();
    assert!(
        error.contains("does not support image inputs"),
        "unexpected image rejection: {error}"
    );
}

#[derive(Clone)]
struct CancellationState {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}

async fn cancellation_resistant_chat(State(state): State<CancellationState>) -> Json<Value> {
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.max_active.fetch_max(active, Ordering::SeqCst);
    state.started.notify_one();

    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let backend_state = state.clone();
    zeroclaw_spawn::spawn!(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        backend_state.active.fetch_sub(1, Ordering::SeqCst);
        let _ = done_tx.send(());
    });
    let _ = done_rx.await;

    Json(json!({
        "message": {"role": "assistant", "content": "ok"},
        "done": true
    }))
}

#[tokio::test]
async fn cancelled_hailo_request_holds_slot_until_backend_finishes() {
    let state = CancellationState {
        active: Arc::new(AtomicUsize::new(0)),
        max_active: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(cancellation_resistant_chat))
        .with_state(state.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = Arc::new(hailo_provider(&format!("http://{addr}")));
    let first_provider = provider.clone();
    let first = zeroclaw_spawn::spawn!(async move {
        first_provider
            .simple_chat("first", "qwen3:1.7b", Some(0.2))
            .await
    });
    state.started.notified().await;
    first.abort();
    let _ = first.await;

    provider
        .simple_chat("second", "qwen3:1.7b", Some(0.2))
        .await
        .expect("second Hailo request succeeds after cancelled first request");
    assert_eq!(
        state.max_active.load(Ordering::SeqCst),
        1,
        "a cancelled request released the Hailo slot before the backend finished"
    );

    server.abort();
}

async fn timeout_surviving_chat(State(state): State<ConcurrencyState>) -> Json<Value> {
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.max_active.fetch_max(active, Ordering::SeqCst);

    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let backend_state = state.clone();
    zeroclaw_spawn::spawn!(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        backend_state.active.fetch_sub(1, Ordering::SeqCst);
        let _ = done_tx.send(());
    });
    let _ = done_rx.await;

    Json(json!({
        "message": {"role": "assistant", "content": "late"},
        "done": true
    }))
}

#[tokio::test]
async fn timed_out_hailo_request_quarantines_provider_without_overlap() {
    let state = ConcurrencyState {
        active: Arc::new(AtomicUsize::new(0)),
        max_active: Arc::new(AtomicUsize::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(timeout_surviving_chat))
        .with_state(state.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });
    let endpoint = format!("http://{addr}");
    let provider = HailoOllamaModelProvider::new(
        "timeout_canary",
        Some(&endpoint),
        1,
        5,
        OllamaTuning {
            num_ctx: 2048,
            num_predict: 64,
            temperature_override: None,
        },
    )
    .expect("valid timeout canary URL");

    provider
        .simple_chat("first", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("first Hailo request must hit its HTTP timeout");
    assert_eq!(
        state.active.load(Ordering::SeqCst),
        1,
        "the backend must still be active when the client timeout becomes ambiguous"
    );
    drop(provider);
    let rebuilt_provider = hailo_provider(&endpoint);
    let second_error = rebuilt_provider
        .simple_chat("second", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("rebuilt provider must retain endpoint quarantine")
        .to_string();

    assert!(
        second_error.contains("quarantined after an ambiguous request timeout"),
        "unexpected post-timeout error: {second_error}"
    );

    server.abort();
}

#[tokio::test]
async fn post_connect_transport_failure_quarantines_endpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind reset server");
    let addr = listener.local_addr().expect("reset server address");
    let endpoint = format!("http://{addr}");
    let server = zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = listener.accept().await.expect("accept Hailo request");
        let mut request_prefix = [0_u8; 1024];
        let _ = stream.read(&mut request_prefix).await;
    });

    let provider = hailo_provider(&endpoint);
    provider
        .simple_chat("first", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("truncated native response must fail");
    server.await.expect("reset server joins");

    let second_error = provider
        .simple_chat("second", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("post-connect failure must quarantine the endpoint")
        .to_string();
    assert!(
        second_error.contains("quarantined after an ambiguous post-connect transport failure"),
        "unexpected post-reset error: {second_error}"
    );
}

#[tokio::test]
async fn connection_establishment_failure_does_not_quarantine_endpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve unused port");
    let addr = listener.local_addr().expect("unused address");
    drop(listener);

    let endpoint = format!("http://{addr}");
    let provider = hailo_provider(&endpoint);
    let first_error = provider
        .simple_chat("first", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("unused port must refuse connection")
        .to_string();
    assert!(
        !first_error.contains(&endpoint),
        "Hailo transport errors must redact endpoint identity: {first_error}"
    );
    assert_eq!(first_error, "Hailo-Ollama connection failed");
    let second_error = provider
        .simple_chat("second", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("second connection must still be attempted")
        .to_string();
    assert!(
        !second_error.contains("quarantined"),
        "connection establishment failure incorrectly quarantined: {second_error}"
    );
    let catalog_error = provider
        .list_models()
        .await
        .expect_err("catalog connection must also fail")
        .to_string();
    assert_eq!(catalog_error, "Hailo-Ollama connection failed");
    assert!(
        !catalog_error.contains(&endpoint),
        "Hailo catalog errors must redact endpoint identity: {catalog_error}"
    );
}

#[derive(Clone)]
struct QueueState {
    started: Arc<tokio::sync::Notify>,
}

async fn long_hailo_chat(State(state): State<QueueState>) -> Json<Value> {
    state.started.notify_one();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    Json(json!({
        "message": {"role": "assistant", "content": "ok"},
        "done": true
    }))
}

#[tokio::test]
async fn native_hailo_queue_wait_is_bounded() {
    let state = QueueState {
        started: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(long_hailo_chat))
        .with_state(state.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = Arc::new(hailo_provider_with_queue_timeout(
        &format!("http://{addr}"),
        1,
    ));
    let first_provider = provider.clone();
    let first = zeroclaw_spawn::spawn!(async move {
        first_provider
            .simple_chat("first", "qwen3:1.7b", Some(0.2))
            .await
    });
    state.started.notified().await;

    let error = provider
        .simple_chat("queued", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("queued Hailo request must time out")
        .to_string();
    assert!(
        error.contains("queue wait timed out at its configured deadline"),
        "unexpected queue timeout: {error}"
    );
    first
        .await
        .expect("first task joins")
        .expect("first request succeeds");

    server.abort();
}

#[tokio::test]
#[ignore = "requires a live Hailo-Ollama endpoint"]
async fn live_native_hailo_catalog_and_chat() {
    let base_url = std::env::var("HAILO_OLLAMA_LIVE_URL")
        .expect("set HAILO_OLLAMA_LIVE_URL for the ignored hardware test");
    let model =
        std::env::var("HAILO_OLLAMA_LIVE_MODEL").unwrap_or_else(|_| "qwen3:1.7b".to_string());
    let provider = HailoOllamaModelProvider::new(
        "live_hardware",
        Some(&base_url),
        90,
        5,
        OllamaTuning {
            num_ctx: 2048,
            num_predict: 64,
            temperature_override: None,
        },
    )
    .expect("valid live Hailo URL");

    let models = provider
        .list_models()
        .await
        .expect("live native Hailo catalog succeeds");
    assert!(
        models.contains(&model),
        "configured live model {model:?} absent from /api/tags: {models:?}"
    );

    let response = provider
        .chat_with_system(
            Some("This is a native provider\ncontract test."),
            "Reply with exactly:\nNATIVE_PROVIDER_OK\nDo not add other text.",
            &model,
            Some(0.0),
        )
        .await
        .expect("live normalized multiline chat succeeds");
    assert_eq!(response.trim(), "NATIVE_PROVIDER_OK");
}
