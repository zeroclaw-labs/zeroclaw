use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::Mutex;
use zeroclaw::config::ObservabilityConfig;
use zeroclaw::observability::{llm_http_trace, runtime_trace};

static TRACE_TEST_LOCK: Mutex<()> = Mutex::new(());

#[tokio::test]
async fn runtime_trace_records_llm_http_request_and_response() {
    let _guard = TRACE_TEST_LOCK.lock().expect("trace test lock");

    let tmp = tempfile::tempdir().expect("tempdir");
    let trace_path = "state/runtime-trace.jsonl".to_string();
    let config = ObservabilityConfig {
        backend: "none".to_string(),
        otel_endpoint: None,
        otel_service_name: None,
        runtime_trace_mode: "full".to_string(),
        runtime_trace_path: trace_path,
        runtime_trace_max_entries: 200,
        runtime_trace_record_http: true,
    };
    runtime_trace::init_from_config(&config, tmp.path());
    llm_http_trace::init_from_config(&config);

    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async { Json(json!({"id":"ok","choices":[]})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server should run");
    });

    let client = reqwest::Client::new();
    let request = client
        .post(format!("http://{addr}/v1/chat/completions?api_key=leak"))
        .header("authorization", "Bearer super-secret-token")
        .json(&json!({"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}));

    let response = llm_http_trace::send_with_middleware("provider.openai", request)
        .await
        .expect("http request should succeed");
    assert!(response.status().is_success());

    let resolved_path = runtime_trace::resolve_trace_path(&config, tmp.path());
    let events = runtime_trace::load_events(&resolved_path, 20, None, None).expect("load traces");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "llm_http_request"),
        "expected llm_http_request event"
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "llm_http_response"),
        "expected llm_http_response event"
    );

    let request_event = events
        .iter()
        .find(|event| event.event_type == "llm_http_request")
        .expect("request event");
    let response_event = events
        .iter()
        .find(|event| event.event_type == "llm_http_response")
        .expect("response event");
    let payload = &request_event.payload;
    assert_eq!(request_event.provider.as_deref(), Some("openai"));

    let headers = payload
        .get("headers")
        .and_then(Value::as_object)
        .expect("headers object");
    assert_eq!(
        headers.get("authorization").and_then(Value::as_str),
        Some("[REDACTED]")
    );
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .expect("url string");
    assert!(
        url.contains("api_key=%5BREDACTED%5D"),
        "query api_key should be redacted"
    );
    assert!(
        response_event.payload.get("content").is_some(),
        "response payload should include raw content"
    );

    server.abort();
    runtime_trace::init_from_config(
        &ObservabilityConfig {
            runtime_trace_mode: "none".to_string(),
            ..ObservabilityConfig::default()
        },
        tmp.path(),
    );
    llm_http_trace::init_from_config(&ObservabilityConfig {
        runtime_trace_mode: "none".to_string(),
        ..ObservabilityConfig::default()
    });
}
