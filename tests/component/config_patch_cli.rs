//! Regression coverage for `zeroclaw config patch --json` error output.

use axum::{Router, routing::patch};
use parking_lot::Mutex;
use std::process::{Command, Stdio};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;
use zeroclaw::gateway::{self, AppState};
use zeroclaw::providers::Provider;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::NoneMemory;
use zeroclaw_runtime::security::PairingGuard;

struct MockProvider;

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("ok".to_string())
    }
}

fn test_state(config: Config) -> AppState {
    AppState {
        config: Arc::new(Mutex::new(config)),
        provider: Arc::new(MockProvider),
        model: "test-model".into(),
        temperature: 0.0,
        mem: Arc::new(NoneMemory::new()),
        auto_save: false,
        webhook_secret_hash: None,
        pairing: Arc::new(PairingGuard::new(false, &[])),
        trust_forwarded_headers: false,
        rate_limiter: Arc::new(gateway::GatewayRateLimiter::new(100, 100, 100)),
        auth_limiter: Arc::new(gateway::auth_rate_limit::AuthRateLimiter::new()),
        idempotency_store: Arc::new(gateway::IdempotencyStore::new(
            Duration::from_secs(300),
            1000,
        )),
        whatsapp: None,
        whatsapp_app_secret: None,
        linq: HashMap::new(),
        linq_signing_secrets: HashMap::new(),
        nextcloud_talk: None,
        nextcloud_talk_webhook_secret: None,
        wati: None,
        gmail_push: None,
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        cost_tracker: None,
        event_tx: tokio::sync::broadcast::channel(16).0,
        event_buffer: Arc::new(gateway::sse::EventBuffer::new(16)),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        reload_tx: None,
        node_registry: Arc::new(gateway::nodes::NodeRegistry::new(16)),
        path_prefix: String::new(),
        web_dist_dir: None,
        session_backend: None,
        session_queue: Arc::new(gateway::session_queue::SessionActorQueue::new(8, 30, 600)),
        device_registry: None,
        pending_pairings: None,
        canvas_store: zeroclaw_runtime::tools::CanvasStore::new(),
        #[cfg(feature = "webauthn")]
        webauthn: None,
        cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    }
}

fn run_cli_patch(config_dir: &std::path::Path, patch_doc: &[u8]) -> serde_json::Value {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    let output = Command::new(bin)
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(["config", "patch", "--json", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .expect("child stdin")
                    .write_all(patch_doc)?;
            }
            child.wait_with_output()
        })
        .expect("run zeroclaw config patch");

    assert!(!output.status.success(), "patch should fail");
    assert!(
        output.stdout.is_empty(),
        "failed --json patch should not emit success stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    serde_json::from_str(&stderr).expect("stderr should be JSON error envelope")
}

async fn run_http_patch(config_dir: &std::path::Path, patch_doc: &[u8]) -> serde_json::Value {
    let config = Config {
        config_path: config_dir.join("config.toml"),
        ..Config::default()
    };
    config.save().await.expect("save initial config");

    let app = Router::new()
        .route("/api/config", patch(gateway::api_config::handle_patch))
        .with_state(test_state(config));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method(axum::http::Method::PATCH)
                .uri("/api/config")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(patch_doc.to_vec()))
                .expect("request"),
        )
        .await
        .expect("http patch response");

    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    serde_json::from_slice(&body).expect("http body should be JSON error envelope")
}

#[tokio::test]
async fn config_patch_json_failed_op_matches_http_error_envelope() {
    let patch_doc = br#"[{"op":"replace","path":"/not/a/path","value":"x"}]"#;
    let cli_config_dir = tempfile::tempdir().expect("temp cli config dir");
    let http_config_dir = tempfile::tempdir().expect("temp http config dir");

    let cli_envelope = run_cli_patch(cli_config_dir.path(), patch_doc);
    let http_envelope = run_http_patch(http_config_dir.path(), patch_doc).await;

    for field in ["code", "path", "op_index"] {
        assert_eq!(
            cli_envelope[field], http_envelope[field],
            "CLI and HTTP mismatch on `{field}`:\nCLI:  {cli_envelope}\nHTTP: {http_envelope}",
        );
    }
    assert_eq!(cli_envelope["code"], "path_not_found");
    assert_eq!(cli_envelope["path"], "not.a.path");
    assert_eq!(cli_envelope["op_index"], 0);
    assert!(
        cli_envelope["message"]
            .as_str()
            .expect("message")
            .contains("not.a.path"),
        "message should identify path: {cli_envelope}"
    );
    assert_eq!(cli_envelope["message"], http_envelope["message"]);
}

#[test]
fn config_patch_json_malformed_operation_emits_structured_error_envelope() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let envelope = run_cli_patch(
        config_dir.path(),
        br#"[{"path":"/gateway/host","value":"x"}]"#,
    );

    assert_eq!(envelope["code"], "value_type_mismatch");
    assert_eq!(envelope["op_index"], 0);
    assert!(envelope.get("path").is_none());
    assert!(
        envelope["message"]
            .as_str()
            .expect("message")
            .contains("requires string `op` field"),
        "message should describe malformed operation: {envelope}"
    );
}

#[test]
fn config_patch_json_post_apply_validation_emits_structured_error_envelope() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let envelope = run_cli_patch(
        config_dir.path(),
        br#"[{"op":"replace","path":"/gateway/host","value":""}]"#,
    );

    assert_eq!(envelope["code"], "required_field_empty");
    assert_eq!(envelope["path"], "gateway.host");
    assert!(envelope.get("op_index").is_none());
    assert!(
        envelope["message"]
            .as_str()
            .expect("message")
            .contains("gateway.host must not be empty"),
        "message should describe validation failure: {envelope}"
    );
}
