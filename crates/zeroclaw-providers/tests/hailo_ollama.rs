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
use zeroclaw_config::schema::{Config, HailoOllamaModelProviderConfig, ModelProviderConfig};
use zeroclaw_providers::hailo_ollama::HailoOllamaModelProvider;
use zeroclaw_providers::ollama::{OllamaModelProvider, OllamaTuning};
use zeroclaw_providers::traits::{
    ChatMessage, ChatRequest, ModelProvider, NonRetryableProviderError,
};
use zeroclaw_providers::{ModelProviderRuntimeOptions, create_model_provider_for_alias_with_url};

type Capture = Arc<Mutex<Option<Value>>>;
type RawCapture = Arc<Mutex<Option<Vec<u8>>>>;
type HeaderCapture = Arc<Mutex<Option<HeaderMap>>>;

// Ignored tests still run in parallel under `cargo test -- --ignored`. Keep
// live canaries for one physical Hailo endpoint out of each other's bounded
// single-flight queue.
static LIVE_HAILO_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

fn hailo_provider_from_public_factory(
    config: &HailoOllamaModelProviderConfig,
    alias: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    options: &ModelProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn ModelProvider>> {
    let mut full_config = Config::default();
    full_config
        .providers
        .models
        .hailo_ollama
        .insert(alias.to_string(), config.clone());
    create_model_provider_for_alias_with_url(
        &full_config,
        "hailo_ollama",
        alias,
        api_key,
        api_url,
        options,
    )
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

async fn count_chat_requests(
    State(requests): State<Arc<AtomicUsize>>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    requests.fetch_add(1, Ordering::SeqCst);
    Json(json!({
        "message": {"role": "assistant", "content": "HAILO_NATIVE_OK"},
        "done": true
    }))
}

async fn catalog_internal_server_error() -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": "catalog temporarily unavailable"})),
    )
}

async fn capture_tags_headers(
    State(capture): State<HeaderCapture>,
    headers: HeaderMap,
) -> Json<Value> {
    *capture.lock().expect("header capture lock") = Some(headers);
    Json(json!({"models": [{"name": "qwen3:1.7b"}]}))
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

async fn emulate_native_hailo_prompt_parser(
    State(capture): State<Capture>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let Some(messages) = body["messages"].as_array() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing messages"})),
        );
    };
    let structured_prompt = messages
        .iter()
        .map(|message| {
            let role = message["role"].as_str().unwrap_or_default();
            let content = message["content"]
                .as_str()
                .unwrap_or_default()
                .replace('"', "\\\"");
            format!(r#"{{"role":"{role}","content":"{content}"}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    let structured_prompt = format!("[{structured_prompt}]");

    match serde_json::from_str::<Value>(&structured_prompt) {
        Ok(decoded) => {
            *capture.lock().expect("prompt capture lock") = Some(decoded);
            (
                StatusCode::OK,
                Json(json!({
                    "message": {"role": "assistant", "content": "HAILO_NATIVE_OK"},
                    "done": true,
                })),
            )
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        ),
    }
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

async fn incomplete_chat() -> Json<Value> {
    Json(json!({
        "message": {"role": "assistant", "content": "partial"},
        "done": false,
    }))
}

async fn missing_done_chat() -> Json<Value> {
    Json(json!({
        "message": {"role": "assistant", "content": "missing marker"},
    }))
}

async fn malformed_success_chat() -> (StatusCode, &'static str) {
    (StatusCode::OK, "{not valid JSON")
}

async fn chat_internal_server_error() -> (StatusCode, &'static str) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"error":"unique-backend-payload"}"#,
    )
}

async fn reflected_sensitive_server_error() -> (StatusCode, &'static str) {
    (
        StatusCode::BAD_GATEWAY,
        r#"{"error":"proxy rejected Bearer gateway-token with route canary"}"#,
    )
}

async fn reflected_sensitive_malformed_success() -> (StatusCode, &'static str) {
    (StatusCode::OK, "{proxy reflected gateway-token and canary")
}

async fn receive_log_event_with_error_key(
    rx: &mut tokio::sync::broadcast::Receiver<Value>,
    error_key: &str,
) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "did not capture log event {error_key}"
        );
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(value))
                if value
                    .get("attributes")
                    .and_then(|attrs| attrs.get("error_key"))
                    .and_then(Value::as_str)
                    == Some(error_key) =>
            {
                return value;
            }
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("log capture closed before {error_key}")
            }
            Err(_) => panic!("timed out waiting for log event {error_key}"),
        }
    }
}

async fn reasoning_only_chat() -> Json<Value> {
    Json(json!({
        "message": {
            "role": "assistant",
            "content": "",
            "thinking": "usable reasoning fallback",
        },
        "done": true,
    }))
}

async fn content_and_reasoning_chat() -> Json<Value> {
    Json(json!({
        "message": {
            "role": "assistant",
            "content": "visible answer",
            "thinking": "internal reasoning",
        },
        "done": true,
    }))
}

async fn empty_chat() -> Json<Value> {
    Json(json!({
        "message": {"role": "assistant", "content": ""},
        "done": true,
        "prompt_eval_count": 7,
        "eval_count": 3,
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

    let provider = OllamaModelProvider::builder("standard")
        .base_url(Some(&format!("http://{addr}")))
        .build();
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
    assert!(body.get("think").is_none());
    assert!(body["options"].get("num_ctx").is_none());
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

async fn redirect_chat_to(
    State(target): State<Arc<Mutex<Option<String>>>>,
) -> axum::response::Response {
    let target = target
        .lock()
        .expect("redirect target lock")
        .clone()
        .expect("redirect target set");
    axum::response::Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(axum::http::header::LOCATION, target)
        .body(axum::body::Body::empty())
        .expect("build redirect response")
}

async fn redirect_tags_to(
    State(target): State<Arc<Mutex<Option<String>>>>,
) -> axum::response::Response {
    let target = target
        .lock()
        .expect("redirect target lock")
        .clone()
        .expect("redirect target set");
    axum::response::Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(axum::http::header::LOCATION, target)
        .body(axum::body::Body::empty())
        .expect("build redirect response")
}

#[tokio::test]
async fn native_hailo_chat_does_not_follow_redirects() {
    // Endpoint B: the real generation backend. If the client followed the
    // 307 from A, this counts the request and captures whatever headers
    // arrived (including any sensitive header that should never leave the
    // configured origin A).
    let b_requests = Arc::new(AtomicUsize::new(0));
    let b_headers: HeaderCapture = Arc::new(Mutex::new(None));
    let b_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo endpoint B");
    let b_addr = b_listener.local_addr().expect("fake Hailo B address");

    async fn count_and_capture_headers(
        State((requests, capture)): State<(Arc<AtomicUsize>, HeaderCapture)>,
        headers: HeaderMap,
        Json(_body): Json<Value>,
    ) -> Json<Value> {
        requests.fetch_add(1, Ordering::SeqCst);
        *capture.lock().expect("header capture lock") = Some(headers);
        Json(json!({
            "message": {"role": "assistant", "content": "HAILO_NATIVE_OK"},
            "done": true
        }))
    }

    let b_app = Router::new()
        .route("/api/chat", post(count_and_capture_headers))
        .with_state((b_requests.clone(), b_headers.clone()));
    let b_server = zeroclaw_spawn::spawn!(async move {
        axum::serve(b_listener, b_app)
            .await
            .expect("serve fake Hailo endpoint B");
    });

    // Endpoint A: the configured, trusted origin. It answers /api/chat with
    // a 307 pointing at B, carrying a sensitive proxy header set only on A.
    let redirect_target: Arc<Mutex<Option<String>>> =
        Arc::new(Mutex::new(Some(format!("http://{b_addr}/api/chat"))));
    let a_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo endpoint A");
    let a_addr = a_listener.local_addr().expect("fake Hailo A address");
    let a_app = Router::new()
        .route("/api/chat", post(redirect_chat_to))
        .with_state(redirect_target);
    let a_server = zeroclaw_spawn::spawn!(async move {
        axum::serve(a_listener, a_app)
            .await
            .expect("serve fake Hailo endpoint A");
    });

    let provider = hailo_provider(&format!("http://{a_addr}"));
    let error = provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("a 307 from the configured endpoint must not be followed");

    assert_eq!(
        b_requests.load(Ordering::SeqCst),
        0,
        "endpoint A's redirect must never be followed to endpoint B"
    );
    assert!(
        b_headers.lock().expect("header capture lock").is_none(),
        "no request (and therefore no header) can reach the redirect target"
    );
    // The client must observe A's own response (a 307 without a JSON chat
    // body), not silently succeed via B.
    assert!(
        !error.to_string().is_empty(),
        "rejecting the redirect must surface as a provider error: {error:#}"
    );

    a_server.abort();
    b_server.abort();
}

#[tokio::test]
async fn native_hailo_catalog_does_not_follow_redirects() {
    let b_requests = Arc::new(AtomicUsize::new(0));
    let b_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo endpoint B");
    let b_addr = b_listener.local_addr().expect("fake Hailo B address");

    async fn count_tags_requests(State(requests): State<Arc<AtomicUsize>>) -> Json<Value> {
        requests.fetch_add(1, Ordering::SeqCst);
        Json(json!({"models": [{"name": "qwen3:1.7b"}]}))
    }

    let b_app = Router::new()
        .route("/api/tags", get(count_tags_requests))
        .with_state(b_requests.clone());
    let b_server = zeroclaw_spawn::spawn!(async move {
        axum::serve(b_listener, b_app)
            .await
            .expect("serve fake Hailo endpoint B");
    });

    let redirect_target: Arc<Mutex<Option<String>>> =
        Arc::new(Mutex::new(Some(format!("http://{b_addr}/api/tags"))));
    let a_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo endpoint A");
    let a_addr = a_listener.local_addr().expect("fake Hailo A address");
    let a_app = Router::new()
        .route("/api/tags", get(redirect_tags_to))
        .with_state(redirect_target);
    let a_server = zeroclaw_spawn::spawn!(async move {
        axum::serve(a_listener, a_app)
            .await
            .expect("serve fake Hailo endpoint A");
    });

    let provider = hailo_provider(&format!("http://{a_addr}"));
    provider
        .list_models()
        .await
        .expect_err("a 307 from the configured catalog endpoint must not be followed");

    assert_eq!(
        b_requests.load(Ordering::SeqCst),
        0,
        "endpoint A's catalog redirect must never be followed to endpoint B"
    );

    a_server.abort();
    b_server.abort();
}

#[tokio::test]
async fn native_hailo_redirect_cannot_bypass_a_different_aliass_endpoint_gate() {
    // Two separately configured aliases: one points directly at endpoint B,
    // the other (A) redirects to B. Both fire chat requests concurrently.
    // Because redirects are now disabled, alias A's request must fail
    // locally (rejected redirect) rather than ever reaching B, so it can
    // never share or race B's single-flight/quarantine gate.
    let b_requests = Arc::new(AtomicUsize::new(0));
    let b_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo endpoint B");
    let b_addr = b_listener.local_addr().expect("fake Hailo B address");
    let b_app = Router::new()
        .route("/api/chat", post(count_chat_requests))
        .with_state(b_requests.clone());
    let b_server = zeroclaw_spawn::spawn!(async move {
        axum::serve(b_listener, b_app)
            .await
            .expect("serve fake Hailo endpoint B");
    });

    let redirect_target: Arc<Mutex<Option<String>>> =
        Arc::new(Mutex::new(Some(format!("http://{b_addr}/api/chat"))));
    let a_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo endpoint A");
    let a_addr = a_listener.local_addr().expect("fake Hailo A address");
    let a_app = Router::new()
        .route("/api/chat", post(redirect_chat_to))
        .with_state(redirect_target);
    let a_server = zeroclaw_spawn::spawn!(async move {
        axum::serve(a_listener, a_app)
            .await
            .expect("serve fake Hailo endpoint A");
    });

    let provider_direct_b = hailo_provider(&format!("http://{b_addr}"));
    let provider_redirecting_a = hailo_provider(&format!("http://{a_addr}"));

    provider_direct_b
        .simple_chat("direct request", "qwen3:1.7b", Some(0.2))
        .await
        .expect("the alias configured directly for B must succeed normally");
    provider_redirecting_a
        .simple_chat("redirected request", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("the alias configured for redirecting A must fail, never silently reach B");

    assert_eq!(
        b_requests.load(Ordering::SeqCst),
        1,
        "B must observe exactly the direct alias's own request, never a forwarded one from A"
    );

    a_server.abort();
    b_server.abort();
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
async fn native_hailo_rejects_call_level_thinking_before_http() {
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
    let error = provider
        .chat(
            zeroclaw_api::model_provider::ChatRequest {
                messages: &messages,
                tools: None,
                thinking: Some(zeroclaw_api::model_provider::NativeThinkingParams {
                    budget_tokens: 1024,
                }),
            },
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect_err("native Hailo must reject call-level thinking before HTTP");
    assert!(error.downcast_ref::<NonRetryableProviderError>().is_some());
    let error = error.to_string();
    assert!(error.contains("thinking"), "unexpected error: {error}");
    assert!(capture.lock().expect("capture lock").is_none());
    server.abort();
}

#[tokio::test]
async fn native_hailo_rejects_incomplete_non_streaming_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new().route("/api/chat", post(incomplete_chat));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    let error = provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("incomplete non-streaming Hailo response must fail");
    assert!(error.to_string().contains("done"));
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>()),
        "incomplete Hailo response must stop wrapper retries: {error:#}"
    );
    let second_error = provider
        .simple_chat("second", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("incomplete response must quarantine the endpoint")
        .to_string();
    assert!(
        second_error.contains("quarantined"),
        "unexpected post-incomplete-response error: {second_error}"
    );
    server.abort();
}

#[tokio::test]
async fn native_hailo_error_logs_use_stable_messages_and_structured_payload_attrs() {
    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();
    while rx.try_recv().is_ok() {}

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind non-2xx Hailo server");
    let addr = listener.local_addr().expect("non-2xx Hailo address");
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(
            listener,
            Router::new().route("/api/chat", post(chat_internal_server_error)),
        )
        .await
        .expect("serve non-2xx Hailo server");
    });
    let _ = hailo_provider(&format!("http://{addr}"))
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("non-2xx response must fail");
    let event = receive_log_event_with_error_key(&mut rx, "hailo_api_error").await;
    assert_eq!(
        event.get("message").and_then(Value::as_str),
        Some("Hailo-Ollama API error response")
    );
    let attrs = event.get("attributes").expect("non-2xx event attributes");
    assert_eq!(attrs.get("status").and_then(Value::as_u64), Some(500));
    assert_eq!(
        attrs.get("body_excerpt").and_then(Value::as_str),
        Some(r#"{"error":"unique-backend-payload"}"#)
    );
    server.abort();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind malformed Hailo server");
    let addr = listener.local_addr().expect("malformed Hailo address");
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(
            listener,
            Router::new().route("/api/chat", post(malformed_success_chat)),
        )
        .await
        .expect("serve malformed Hailo server");
    });
    let _ = hailo_provider(&format!("http://{addr}"))
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("malformed response must fail");
    let event = receive_log_event_with_error_key(&mut rx, "hailo_response_deserialize").await;
    assert_eq!(
        event.get("message").and_then(Value::as_str),
        Some("Hailo-Ollama response deserialization failed")
    );
    let attrs = event.get("attributes").expect("malformed event attributes");
    assert_eq!(
        attrs.get("body_excerpt").and_then(Value::as_str),
        Some("{not valid JSON")
    );
    assert!(attrs.get("error").and_then(Value::as_str).is_some());
    server.abort();
    zeroclaw_log::clear_broadcast_hook();
}

#[tokio::test]
async fn native_hailo_error_diagnostics_redact_configured_auth_header_values() {
    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();
    while rx.try_recv().is_ok() {}

    let options = ModelProviderRuntimeOptions {
        extra_headers: [("X-Route".to_string(), "canary".to_string())]
            .into_iter()
            .collect(),
        ..Default::default()
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind reflected-error Hailo server");
    let addr = listener
        .local_addr()
        .expect("reflected-error Hailo address");
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(
            listener,
            Router::new().route("/api/chat", post(reflected_sensitive_server_error)),
        )
        .await
        .expect("serve reflected-error Hailo server");
    });
    let provider = hailo_provider_from_public_factory(
        &HailoOllamaModelProviderConfig::default(),
        "reflected_error",
        Some("gateway-token"),
        Some(&format!("http://{addr}")),
        &options,
    )
    .expect("authenticated Hailo alias should build");
    let error = provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("reflected non-2xx response must fail")
        .to_string();
    assert!(!error.contains("gateway-token"));
    assert!(!error.contains("canary"));
    let event = receive_log_event_with_error_key(&mut rx, "hailo_api_error").await;
    let body_excerpt = event["attributes"]["body_excerpt"]
        .as_str()
        .expect("non-2xx body excerpt");
    assert!(!body_excerpt.contains("gateway-token"));
    assert!(!body_excerpt.contains("canary"));
    assert!(body_excerpt.contains("[REDACTED]"));
    server.abort();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind reflected-malformed Hailo server");
    let addr = listener
        .local_addr()
        .expect("reflected-malformed Hailo address");
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(
            listener,
            Router::new().route("/api/chat", post(reflected_sensitive_malformed_success)),
        )
        .await
        .expect("serve reflected-malformed Hailo server");
    });
    let provider = hailo_provider_from_public_factory(
        &HailoOllamaModelProviderConfig::default(),
        "reflected_malformed",
        Some("gateway-token"),
        Some(&format!("http://{addr}")),
        &options,
    )
    .expect("authenticated Hailo alias should build");
    let _ = provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("reflected malformed response must fail");
    let event = receive_log_event_with_error_key(&mut rx, "hailo_response_deserialize").await;
    let body_excerpt = event["attributes"]["body_excerpt"]
        .as_str()
        .expect("malformed body excerpt");
    assert!(!body_excerpt.contains("gateway-token"));
    assert!(!body_excerpt.contains("canary"));
    assert!(body_excerpt.contains("[REDACTED]"));
    server.abort();
    zeroclaw_log::clear_broadcast_hook();
}

#[tokio::test]
async fn native_hailo_rejects_response_without_done_marker() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new().route("/api/chat", post(missing_done_chat));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    let error = provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("response without done marker must fail");
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>()),
        "missing done marker must stop wrapper retries: {error:#}"
    );
    let second_error = provider
        .simple_chat("second", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("missing done marker must quarantine the endpoint")
        .to_string();
    assert!(second_error.contains("quarantined"));
    server.abort();
}

#[tokio::test]
async fn native_hailo_quarantines_malformed_success_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new().route("/api/chat", post(malformed_success_chat));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    let error = provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("malformed successful response must fail");
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>()),
        "malformed successful response must stop wrapper retries: {error:#}"
    );
    let second_error = provider
        .simple_chat("second", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("malformed successful response must quarantine the endpoint")
        .to_string();
    assert!(second_error.contains("quarantined"));
    server.abort();
}

#[tokio::test]
async fn native_hailo_accepts_nonempty_reasoning_when_content_is_empty() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new().route("/api/chat", post(reasoning_only_chat));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let response = hailo_provider(&format!("http://{addr}"))
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect("nonempty reasoning is an intentional usable fallback");
    assert_eq!(response, "usable reasoning fallback");
    server.abort();
}

#[tokio::test]
async fn native_hailo_prefers_content_over_reasoning() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new().route("/api/chat", post(content_and_reasoning_chat));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let response = hailo_provider(&format!("http://{addr}"))
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect("content response succeeds");
    assert_eq!(response, "visible answer");
    server.abort();
}

#[tokio::test]
async fn native_hailo_rejects_empty_completed_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new().route("/api/chat", post(empty_chat));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let error = hailo_provider(&format!("http://{addr}"))
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("empty completed Hailo response must fail")
        .to_string();
    assert!(error.contains("empty"), "unexpected error: {error}");
    server.abort();
}

#[tokio::test]
async fn native_hailo_preserves_usage_for_empty_completed_chat_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new().route("/api/chat", post(empty_chat));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let messages = [ChatMessage::user("hello")];
    let response = hailo_provider(&format!("http://{addr}"))
        .chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect("empty completed response must retain its billed usage for reliability");
    assert!(response.is_semantically_empty_terminal());
    let usage = response.usage.expect("provider token counts must survive");
    assert_eq!(usage.input_tokens, Some(7));
    assert_eq!(usage.output_tokens, Some(3));
    server.abort();
}

#[tokio::test]
async fn native_hailo_uses_context_window_for_local_history_budget_only() {
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

    let provider = HailoOllamaModelProvider::new(
        "small_context",
        Some(&format!("http://{addr}")),
        5,
        5,
        OllamaTuning {
            num_ctx: 256,
            num_predict: 64,
            temperature_override: None,
        },
    )
    .expect("valid fake Hailo URL");
    let messages = [
        ChatMessage::user("old ".repeat(300)),
        ChatMessage::assistant("old answer ".repeat(300)),
        ChatMessage::user("middle ".repeat(300)),
        ChatMessage::assistant("middle answer ".repeat(300)),
        ChatMessage::user("LATEST_CONTEXT_TAIL"),
    ];
    provider
        .chat_with_history(&messages, "qwen3:1.7b", Some(0.2))
        .await
        .expect("bounded local history request succeeds");

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    let request_messages = body["messages"].as_array().expect("messages array");
    assert!(request_messages.len() < messages.len());
    assert!(
        request_messages
            .last()
            .and_then(|message| message["content"].as_str())
            .is_some_and(|content| content.contains("LATEST_CONTEXT_TAIL"))
    );
    assert!(body["options"].get("num_ctx").is_none());
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
async fn native_hailo_preserves_literal_backslashes_through_prompt_parser() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(emulate_native_hailo_prompt_parser))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let prompt = concat!(
        r#"regex \d+; literal \n and \u263a; path C:\temp\file; escaped quote \"ok\"; "#,
        "actual\r\n\tcontrols"
    );
    hailo_provider(&format!("http://{addr}"))
        .simple_chat(prompt, "qwen3:1.7b", Some(0.2))
        .await
        .expect("native Hailo prompt parser accepts literal backslashes");

    let decoded = capture
        .lock()
        .expect("prompt capture lock")
        .clone()
        .expect("structured prompt captured");
    assert_eq!(decoded[0]["content"], prompt);
    server.abort();
}

#[tokio::test]
async fn native_hailo_truncates_without_splitting_prompt_escape_units() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(emulate_native_hailo_prompt_parser))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let prompt = "\\".repeat(1_001);
    hailo_provider(&format!("http://{addr}"))
        .simple_chat(&prompt, "qwen3:1.7b", Some(0.2))
        .await
        .expect("native prompt truncation must preserve complete escape units");

    let decoded = capture
        .lock()
        .expect("prompt capture lock")
        .clone()
        .expect("structured prompt captured");
    let expected = format!("{}...{}", "\\".repeat(499), "\\".repeat(499));
    assert_eq!(decoded[0]["content"], expected);
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

    let provider = OllamaModelProvider::builder("standard")
        .base_url(Some(&format!("http://{addr}")))
        .build();
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
        .expect_err("native Hailo tools must be rejected");

    assert!(
        error
            .to_string()
            .contains("does not support native tool calling")
    );
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>()),
        "a permanent native-tool capability mismatch must suppress retries: {error:#}"
    );
    assert!(capture.lock().expect("capture lock").is_none());
    server.abort();
}

#[tokio::test]
async fn native_hailo_preserves_fitting_tool_protocol_before_bounding_user_text() {
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
        "## Tool Use Protocol\n{}\nTOOL_PROTOCOL_END",
        "Use tool_call exactly. ".repeat(45)
    );
    assert!(system.chars().count() < 2_000);
    let user = format!("USER_HEAD{}USER_TAIL", "u".repeat(1_800));
    let messages = [ChatMessage::system(system), ChatMessage::user(user)];

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
        .expect("a fitting complete tool protocol should survive by bounding user text first");

    let body = capture
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request captured");
    let content = body["messages"][0]["content"]
        .as_str()
        .expect("folded first message");
    assert!(content.contains("TOOL_PROTOCOL_END"));
    assert!(content.contains("USER_HEAD"));
    assert!(content.contains("USER_TAIL"));
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
        .expect_err("oversized prompt-guided tools must fail closed");

    assert!(
        error
            .to_string()
            .contains("prompt-guided tool instructions exceed"),
        "unexpected tool prompt error: {error}"
    );
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>()),
        "a deterministic local prompt rejection must suppress generic retries: {error:#}"
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
async fn native_hailo_fails_closed_when_latest_tool_turn_exceeds_context_budget() {
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
    let provider = HailoOllamaModelProvider::new(
        "edge",
        Some(&format!("http://{addr}")),
        5,
        5,
        OllamaTuning {
            num_ctx: 64,
            num_predict: 64,
            temperature_override: None,
        },
    )
    .expect("valid fake Hailo URL");
    let history = vec![
        ChatMessage::user("Read README.md"),
        ChatMessage::assistant(
            r#"{"content":null,"tool_calls":[{"id":"call_1","name":"file_read","arguments":"{\"path\":\"README.md\"}"}]}"#,
        ),
        ChatMessage::tool("LATEST_TOOL_RESULT"),
    ];

    let error = provider
        .chat_with_history(&history, "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("an unrepresentable latest tool turn must fail before HTTP");

    assert!(
        error.to_string().contains("local context budget"),
        "unexpected context-budget error: {error:#}"
    );
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>()),
        "an impossible local context budget must suppress generic retries: {error:#}"
    );
    assert!(
        capture.lock().expect("capture lock").is_none(),
        "the provider must not replace a lost tool turn with a synthetic request"
    );
    server.abort();
}

#[tokio::test]
async fn native_hailo_fails_closed_when_folded_system_exceeds_context_budget() {
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

    let provider = HailoOllamaModelProvider::new(
        "system-budget",
        Some(&format!("http://{addr}")),
        5,
        5,
        OllamaTuning {
            num_ctx: 300,
            num_predict: 256,
            temperature_override: None,
        },
    )
    .expect("valid fake Hailo URL");

    let error = provider
        .chat_with_history(
            &[
                ChatMessage::system("S".repeat(1_000)),
                ChatMessage::user("Run the check"),
            ],
            "qwen2:1.5b",
            Some(0.2),
        )
        .await
        .expect_err("the folded system prompt must count against the local context budget");
    assert!(error.to_string().contains("local context budget"));
    assert!(
        capture.lock().expect("capture lock").is_none(),
        "the provider must reject the folded prompt before contacting Hailo"
    );

    server.abort();
}

#[tokio::test]
async fn native_hailo_fails_closed_when_message_cap_splits_latest_tool_turn() {
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

    let mut history = vec![ChatMessage::user("Run every requested check")];
    for index in 0..6 {
        history.push(ChatMessage::assistant(
            json!({
                "content": null,
                "tool_calls": [{
                    "id": format!("call_{index}"),
                    "name": "shell",
                    "arguments": "{\"command\":\"true\"}"
                }]
            })
            .to_string(),
        ));
        history.push(ChatMessage::tool(format!("LATEST_TOOL_RESULT_{index}")));
    }

    let error = hailo_provider(&format!("http://{addr}"))
        .chat_with_history(&history, "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("a latest turn split by the message cap must fail before HTTP");

    assert!(
        error.to_string().contains("local history message budget"),
        "unexpected message-budget error: {error:#}"
    );
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>()),
        "an impossible local message budget must suppress generic retries: {error:#}"
    );
    assert!(
        capture.lock().expect("capture lock").is_none(),
        "the provider must not replace a split tool turn with a synthetic request"
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
async fn failed_catalog_does_not_quarantine_hailo_chat_endpoint() {
    let chat_requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/tags", get(catalog_internal_server_error))
        .route("/api/chat", post(count_chat_requests))
        .with_state(chat_requests.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    provider
        .list_models()
        .await
        .expect_err("a failed Hailo catalog must remain an error");
    provider
        .simple_chat("still healthy", "qwen3:1.7b", Some(0.2))
        .await
        .expect("a catalog failure must not block a later Hailo chat request");
    assert_eq!(chat_requests.load(Ordering::SeqCst), 1);

    server.abort();
}

#[tokio::test]
async fn native_hailo_rejects_history_without_a_user_anchor() {
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

    let error = hailo_provider(&format!("http://{addr}"))
        .chat_with_history(
            &[
                ChatMessage::system("Preserve the real request."),
                ChatMessage::assistant("orphaned assistant prefill"),
                ChatMessage::tool("orphaned tool result"),
            ],
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect_err("orphan-only history must fail instead of synthesizing a user request");

    assert!(error.to_string().contains("user-anchored turn"));
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>())
    );
    assert!(capture.lock().expect("capture lock").is_none());

    server.abort();
}

#[tokio::test]
async fn native_hailo_accounts_for_merge_separators_in_context_budget() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let provider = HailoOllamaModelProvider::new(
        "hailo_ollama.edge",
        Some(&format!("http://{addr}")),
        30,
        30,
        OllamaTuning {
            num_ctx: 264,
            num_predict: 256,
            ..Default::default()
        },
    )
    .unwrap();
    let messages = vec![
        ChatMessage::user("uuu"),
        ChatMessage::assistant("a"),
        ChatMessage::tool("x"),
        ChatMessage::tool("x"),
    ];

    let error = provider
        .chat_with_history(&messages, "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("wire merge separators must count toward the context budget");
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>())
    );
    assert!(capture.lock().expect("capture lock").is_none());
    server.abort();
}

#[tokio::test]
async fn native_hailo_rejects_prompt_tool_rounds_over_message_cap() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let provider = HailoOllamaModelProvider::new(
        "hailo_ollama.edge",
        Some(&format!("http://{addr}")),
        30,
        30,
        OllamaTuning::default(),
    )
    .unwrap();
    let mut messages = vec![ChatMessage::user("ACTIVE_USER_INSTRUCTION")];
    for round in 0..6 {
        messages.push(ChatMessage::assistant(format!(
            "<tool_call>round {round}</tool_call>"
        )));
        messages.push(ChatMessage::user(format!(
            "[Tool results]\n<tool_result>result {round}</tool_result>"
        )));
    }
    let error = provider
        .chat_with_history(&messages, "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("the active prompt-tool turn must not be split at the message cap");
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>())
    );
    assert!(capture.lock().expect("capture lock").is_none());
    server.abort();
}

#[tokio::test]
async fn native_hailo_rejects_prompt_tool_rounds_over_context_budget() {
    let capture: Capture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/api/chat", post(capture_chat))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let provider = HailoOllamaModelProvider::new(
        "hailo_ollama.edge",
        Some(&format!("http://{addr}")),
        30,
        30,
        OllamaTuning {
            num_ctx: 600,
            num_predict: 256,
            ..Default::default()
        },
    )
    .unwrap();
    let mut messages = vec![ChatMessage::user("ACTIVE_USER_INSTRUCTION")];
    for round in 0..2 {
        messages.push(ChatMessage::assistant(format!(
            "<tool_call>round {round}</tool_call>"
        )));
        messages.push(ChatMessage::user(format!(
            "[Tool results]\n<tool_result>{}</tool_result>",
            "x".repeat(900)
        )));
    }
    let error = provider
        .chat_with_history(&messages, "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("the active prompt-tool turn must not be split at the context budget");
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>())
    );
    assert!(capture.lock().expect("capture lock").is_none());
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
async fn typed_hailo_factory_keeps_context_tokens_off_the_wire_and_applies_timeout_and_alias() {
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
    let provider = hailo_provider_from_public_factory(
        &config,
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
    assert!(body["options"].get("num_ctx").is_none());
    assert_eq!(body["options"]["num_predict"], 96);

    server.abort();
}

#[tokio::test]
async fn typed_hailo_factory_applies_auth_and_headers_to_chat_and_catalog() {
    let config = HailoOllamaModelProviderConfig::default();
    let capture: HeaderCapture = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/api/chat", post(capture_chat_headers))
        .route("/api/tags", get(capture_tags_headers))
        .with_state(capture.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let provider = hailo_provider_from_public_factory(
        &config,
        "edge",
        Some("gateway-token"),
        Some(&format!("http://{addr}")),
        &ModelProviderRuntimeOptions {
            extra_headers: [("X-Route".to_string(), "canary".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    )
    .expect("authenticated Hailo alias should build");
    provider
        .simple_chat("hello", "qwen3:1.7b", Some(0.2))
        .await
        .unwrap();
    let chat_headers = capture.lock().unwrap().take().unwrap();
    assert_eq!(
        chat_headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        Some("Bearer gateway-token")
    );
    assert_eq!(
        chat_headers.get("x-route").and_then(|v| v.to_str().ok()),
        Some("canary")
    );
    assert_eq!(provider.list_models().await.unwrap(), vec!["qwen3:1.7b"]);
    let catalog_headers = capture.lock().unwrap().take().unwrap();
    assert_eq!(
        catalog_headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        Some("Bearer gateway-token")
    );
    assert_eq!(
        catalog_headers.get("x-route").and_then(|v| v.to_str().ok()),
        Some("canary")
    );
    server.abort();
}

#[test]
fn typed_hailo_factory_rejects_unsupported_capabilities() {
    let config = HailoOllamaModelProviderConfig::default();
    let cases = [
        (
            "thinking",
            ModelProviderRuntimeOptions {
                think: Some(true),
                ..Default::default()
            },
        ),
        (
            "native_tools",
            ModelProviderRuntimeOptions {
                native_tools: Some(true),
                ..Default::default()
            },
        ),
    ];
    for (capability, options) in cases {
        let error = hailo_provider_from_public_factory(
            &config,
            "unsupported_capability",
            None,
            Some("http://127.0.0.1:8000"),
            &options,
        )
        .err()
        .expect("unsupported Hailo capability must be rejected");
        let typed = error
            .downcast_ref::<zeroclaw_providers::ProviderCapabilityError>()
            .unwrap_or_else(|| panic!("{capability} rejection must remain typed: {error:#}"));
        assert_eq!(typed.model_provider, "unsupported_capability");
        assert_eq!(typed.capability, capability);
    }
}

#[test]
fn typed_hailo_factory_rejects_conflicting_authorization_sources() {
    let config = HailoOllamaModelProviderConfig::default();
    let options = ModelProviderRuntimeOptions {
        extra_headers: [(
            "aUtHoRiZaTiOn".to_string(),
            "Bearer shadow-token".to_string(),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let error = hailo_provider_from_public_factory(
        &config,
        "conflicting_auth",
        Some("canonical-token"),
        Some("http://127.0.0.1:8000"),
        &options,
    )
    .err()
    .expect("ambiguous Hailo authentication sources must be rejected");
    let message = error.to_string();
    assert!(message.contains("Authorization"));
    assert!(!message.contains("canonical-token"));
    assert!(!message.contains("shadow-token"));
    hailo_provider_from_public_factory(
        &config,
        "header_only_auth",
        None,
        Some("http://127.0.0.1:8000"),
        &options,
    )
    .expect("header-only Hailo authentication must remain supported");
}

#[test]
fn typed_hailo_factory_rejects_case_insensitive_duplicate_extra_headers() {
    let config = HailoOllamaModelProviderConfig::default();
    let options = ModelProviderRuntimeOptions {
        extra_headers: [
            (
                "Authorization".to_string(),
                "Bearer first-token".to_string(),
            ),
            (
                "authorization".to_string(),
                "Bearer second-token".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };

    let error = hailo_provider_from_public_factory(
        &config,
        "duplicate_extra_headers",
        None,
        Some("http://127.0.0.1:8000"),
        &options,
    )
    .err()
    .expect("case-insensitive duplicate extra headers must be rejected");
    let message = error.to_string();
    assert!(message.contains("duplicate"));
    assert!(message.contains("case-insensitive"));
    assert!(!message.contains("first-token"));
    assert!(!message.contains("second-token"));
}

#[test]
fn typed_hailo_factory_rejects_unsupported_shared_options() {
    let config = HailoOllamaModelProviderConfig::default();
    let cases = [
        (
            "tls_ca_cert_path",
            ModelProviderRuntimeOptions {
                tls_ca_cert_path: Some("/tmp/private-ca.pem".to_string()),
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
        let error = match hailo_provider_from_public_factory(
            &config,
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

#[test]
fn production_factory_rejects_hailo_vision_override() {
    let options = ModelProviderRuntimeOptions {
        vision: Some(true),
        ..Default::default()
    };

    let direct_error = hailo_provider_from_public_factory(
        &HailoOllamaModelProviderConfig::default(),
        "hailo_ollama",
        None,
        None,
        &options,
    )
    .err()
    .expect("text-only Hailo family factory must reject vision=true");
    assert!(
        direct_error
            .downcast_ref::<zeroclaw_providers::ProviderCapabilityError>()
            .is_some(),
        "family factory must return a typed capability error: {direct_error:?}"
    );

    let error = match zeroclaw_providers::create_model_provider_with_options(
        "hailo_ollama",
        None,
        &options,
    ) {
        Ok(_) => panic!("text-only Hailo must reject vision=true"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("vision"),
        "vision missing from error: {error}"
    );
    let capability = error
        .downcast_ref::<zeroclaw_providers::ProviderCapabilityError>()
        .expect("vision override rejection must remain a typed capability error");
    assert_eq!(capability.model_provider, "default");
    assert_eq!(capability.capability, "vision");
}

#[tokio::test]
async fn native_hailo_rejects_image_inputs_instead_of_dropping_them() {
    // A valid opaque 1x1 RGB PNG containing one red pixel (RGB 255, 0, 0).
    const ONE_PIXEL_RED_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
    let requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(count_chat_requests))
        .with_state(requests.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    let prompt = format!("Describe this [IMAGE:data:image/png;base64,{ONE_PIXEL_RED_PNG_B64}]");
    let error = provider
        .simple_chat(&prompt, "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("native Hailo image input must fail before HTTP");
    assert!(error.to_string().contains("does not support image inputs"));
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>())
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    server.abort();
}

#[tokio::test]
async fn native_hailo_rejects_unloadable_image_markers_before_http() {
    let requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Hailo server");
    let addr = listener.local_addr().expect("fake Hailo address");
    let app = Router::new()
        .route("/api/chat", post(count_chat_requests))
        .with_state(requests.clone());
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Hailo server");
    });

    let provider = hailo_provider(&format!("http://{addr}"));
    let error = provider
        .simple_chat(
            "Describe this [IMAGE:/definitely/not/here.png]",
            "qwen3:1.7b",
            Some(0.2),
        )
        .await
        .expect_err("unloadable image marker must fail instead of becoming plain text");
    assert!(error.to_string().contains("does not support image inputs"));
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>())
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    server.abort();
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

    let first_error = provider
        .simple_chat("first", "qwen3:1.7b", Some(0.2))
        .await
        .expect_err("first Hailo request must hit its HTTP timeout");
    assert!(
        first_error
            .downcast_ref::<NonRetryableProviderError>()
            .is_some(),
        "ambiguous timeout must retain its non-retryable classification: {first_error:?}"
    );
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
        .expect_err("queued Hailo request must time out");
    assert!(
        error
            .chain()
            .any(|source| source.is::<NonRetryableProviderError>()),
        "a queue deadline must suppress reliability retries: {error:#}"
    );
    let error = error.to_string();
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
    let _live_hardware_guard = LIVE_HAILO_TEST_LOCK.lock().await;
    let base_url = std::env::var("HAILO_OLLAMA_LIVE_URL")
        .expect("set HAILO_OLLAMA_LIVE_URL for the ignored hardware test");
    let model =
        std::env::var("HAILO_OLLAMA_LIVE_MODEL").unwrap_or_else(|_| "qwen3:1.7b".to_string());
    let provider = HailoOllamaModelProvider::new(
        "live_hardware",
        Some(&base_url),
        240,
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

#[tokio::test]
#[ignore = "requires a live Hailo-Ollama endpoint"]
async fn live_native_hailo_accepts_prompt_escape_corner_cases() {
    let _live_hardware_guard = LIVE_HAILO_TEST_LOCK.lock().await;
    let base_url = std::env::var("HAILO_OLLAMA_LIVE_URL")
        .expect("set HAILO_OLLAMA_LIVE_URL for the ignored hardware test");
    let model =
        std::env::var("HAILO_OLLAMA_LIVE_MODEL").unwrap_or_else(|_| "qwen3:1.7b".to_string());
    let provider = HailoOllamaModelProvider::new(
        "live_escape_hardware",
        Some(&base_url),
        240,
        5,
        OllamaTuning {
            num_ctx: 2048,
            num_predict: 32,
            temperature_override: None,
        },
    )
    .expect("valid live Hailo URL");

    let cases = [
        (
            "literal escapes",
            r#"Regex \d+; literal \n and \u263a; path C:\temp\file; quote \"ok\". Reply briefly."#
                .to_string(),
        ),
        (
            "escape-pair truncation boundary",
            format!(
                "{} Reply briefly to confirm this request was accepted.",
                "\\".repeat(1_001)
            ),
        ),
    ];

    for (name, prompt) in cases {
        let response = provider
            .simple_chat(&prompt, &model, Some(0.0))
            .await
            .unwrap_or_else(|error| panic!("live {name} request failed: {error}"));
        assert!(!response.trim().is_empty(), "live {name} response is empty");
    }
}

#[tokio::test]
#[ignore = "requires a live Hailo-Ollama endpoint"]
async fn live_native_hailo_remains_usable_after_a_completed_chat() {
    let _live_hardware_guard = LIVE_HAILO_TEST_LOCK.lock().await;
    let base_url = std::env::var("HAILO_OLLAMA_LIVE_URL")
        .expect("set HAILO_OLLAMA_LIVE_URL for the ignored hardware test");
    let model =
        std::env::var("HAILO_OLLAMA_LIVE_MODEL").unwrap_or_else(|_| "qwen3:1.7b".to_string());
    let provider = HailoOllamaModelProvider::new(
        "live_recovery_hardware",
        Some(&base_url),
        240,
        5,
        OllamaTuning {
            num_ctx: 2048,
            num_predict: 32,
            temperature_override: None,
        },
    )
    .expect("valid live Hailo URL");

    for prompt in [
        "Reply with exactly: NATIVE_RECOVERY_FIRST",
        "Reply with exactly: NATIVE_RECOVERY_SECOND",
    ] {
        let response = provider
            .simple_chat(prompt, &model, Some(0.0))
            .await
            .unwrap_or_else(|error| panic!("live recovery chat failed: {error}"));
        assert!(
            !response.trim().is_empty(),
            "live recovery response is empty"
        );
    }
}
