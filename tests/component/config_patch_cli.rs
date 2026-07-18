//! Regression coverage for `zeroclaw config patch --json` output.

use axum::{Router, routing::patch};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;
use zeroclaw::gateway::{self, AppState};
use zeroclaw_api::attribution::Attributable;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::NoneMemory;
use zeroclaw_providers::ModelProvider;
use zeroclaw_runtime::security::PairingGuard;

#[derive(Default)]
struct MockModelProvider;

#[async_trait::async_trait]
impl ModelProvider for MockModelProvider {
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

impl Attributable for MockModelProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }

    fn alias(&self) -> &str {
        "MockModelProvider"
    }
}

fn test_state(config: Config) -> AppState {
    let memory: Arc<dyn zeroclaw_memory::Memory> =
        Arc::new(NoneMemory::new("config-patch-cli-test"));
    AppState {
        config: Arc::new(RwLock::new(config)),
        model_provider: Arc::new(MockModelProvider),
        model: "test-model".into(),
        temperature: None,
        mem: memory.clone(),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                memory,
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
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
        #[cfg(feature = "channel-whatsapp-cloud")]
        whatsapp: HashMap::new(),
        #[cfg(feature = "channel-whatsapp-cloud")]
        whatsapp_app_secret: HashMap::new(),
        #[cfg(feature = "channel-linq")]
        linq: HashMap::new(),
        #[cfg(feature = "channel-linq")]
        linq_signing_secrets: HashMap::new(),
        #[cfg(feature = "channel-nextcloud")]
        nextcloud_talk: HashMap::new(),
        #[cfg(feature = "channel-nextcloud")]
        nextcloud_talk_webhook_secret: HashMap::new(),
        #[cfg(feature = "channel-wati")]
        wati: HashMap::new(),
        #[cfg(feature = "channel-email")]
        gmail_push: None,
        observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
        tools_registry: Arc::new(Vec::new()),
        tools_registry_by_agent: Arc::new(HashMap::new()),
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
        pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        tui_registry: None,
        sop_engine: None,
        sop_audit: None,
    }
}

fn run_cli_patch_output(config_dir: &std::path::Path, patch_doc: &[u8]) -> Output {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    Command::new(bin)
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
        .expect("run zeroclaw config patch")
}

fn run_cli_patch(config_dir: &std::path::Path, patch_doc: &[u8]) -> serde_json::Value {
    let output = run_cli_patch_output(config_dir, patch_doc);
    assert!(!output.status.success(), "patch should fail");
    assert!(
        output.stdout.is_empty(),
        "failed --json patch should not emit success stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    serde_json::from_str(&stderr).expect("stderr should be JSON error envelope")
}

fn run_cli_patch_success(config_dir: &std::path::Path, patch_doc: &[u8]) -> serde_json::Value {
    let output = run_cli_patch_output(config_dir, patch_doc);
    assert!(output.status.success(), "patch should succeed");
    assert!(
        output.stderr.is_empty(),
        "successful --json patch should not emit stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    serde_json::from_str(&stdout).expect("stdout should be JSON success envelope")
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

#[test]
fn config_patch_json_success_emits_envelope_and_persists_change() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let envelope = run_cli_patch_success(
        config_dir.path(),
        br#"[{"op":"replace","path":"/gateway/host","value":"127.0.0.2"}]"#,
    );

    assert_eq!(envelope["saved"], true);
    assert_eq!(envelope["results"][0]["op"], "replace");
    assert_eq!(envelope["results"][0]["path"], "gateway.host");
    assert_eq!(envelope["results"][0]["value"], "127.0.0.2");

    let saved =
        std::fs::read_to_string(config_dir.path().join("config.toml")).expect("read saved config");
    let parsed: Config = toml::from_str(&saved).expect("saved config should parse");
    assert_eq!(parsed.gateway.host, "127.0.0.2");
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

// ─────────────────────────────────────────────────────────────────────────────
// Alias auto-materialization (mirrors `PATCH /api/config`'s
// `ensure_map_key_for_path` guard in `handle_patch`, api_config.rs:2040-2051)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_patch_add_materializes_new_map_alias_and_persists() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let envelope = run_cli_patch_success(
        config_dir.path(),
        br#"[
            {"op":"add","path":"/channels/telegram/newbot/enabled","value":true},
            {"op":"add","path":"/channels/telegram/newbot/bot_token","value":"dummy-token"}
        ]"#,
    );

    assert_eq!(envelope["saved"], true);
    assert_eq!(envelope["results"][0]["op"], "add");
    assert_eq!(
        envelope["results"][0]["path"],
        "channels.telegram.newbot.enabled"
    );
    assert_eq!(envelope["results"][0]["value"], "true");

    let saved =
        std::fs::read_to_string(config_dir.path().join("config.toml")).expect("read saved config");
    let parsed: Config = toml::from_str(&saved).expect("saved config should parse");
    let newbot = parsed
        .channels
        .telegram
        .get("newbot")
        .expect("new alias should be persisted to disk");
    assert!(
        newbot.enabled,
        "materialized alias should carry the patched value"
    );
}

#[test]
fn config_patch_replace_materializes_new_map_alias_and_persists() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let envelope = run_cli_patch_success(
        config_dir.path(),
        br#"[
            {"op":"replace","path":"/channels/telegram/anotherbot/enabled","value":true},
            {"op":"replace","path":"/channels/telegram/anotherbot/bot_token","value":"dummy-token"}
        ]"#,
    );

    assert_eq!(envelope["saved"], true);
    assert_eq!(envelope["results"][0]["op"], "replace");
    assert_eq!(
        envelope["results"][0]["path"],
        "channels.telegram.anotherbot.enabled"
    );

    let saved =
        std::fs::read_to_string(config_dir.path().join("config.toml")).expect("read saved config");
    let parsed: Config = toml::from_str(&saved).expect("saved config should parse");
    let bot = parsed
        .channels
        .telegram
        .get("anotherbot")
        .expect("new alias should be persisted to disk via `replace` too");
    assert!(bot.enabled);
}

#[test]
fn config_patch_replace_on_existing_alias_does_not_recreate_it() {
    let config_dir = tempfile::tempdir().expect("temp config dir");

    // Establish the alias first.
    run_cli_patch_success(
        config_dir.path(),
        br#"[
            {"op":"add","path":"/channels/telegram/existingbot/enabled","value":true},
            {"op":"add","path":"/channels/telegram/existingbot/bot_token","value":"dummy-token"}
        ]"#,
    );
    let saved_before = std::fs::read_to_string(config_dir.path().join("config.toml"))
        .expect("read config after setup");
    let before: Config = toml::from_str(&saved_before).expect("config should parse");
    let mut keys_before = before
        .get_map_keys("channels.telegram")
        .expect("channels.telegram should be a map-keyed section");
    keys_before.sort();

    let envelope = run_cli_patch_success(
        config_dir.path(),
        br#"[{"op":"replace","path":"/channels/telegram/existingbot/enabled","value":false}]"#,
    );
    assert_eq!(envelope["results"][0]["value"], "false");

    let saved_after = std::fs::read_to_string(config_dir.path().join("config.toml"))
        .expect("read config after replace");
    let after: Config = toml::from_str(&saved_after).expect("config should parse");
    let mut keys_after = after
        .get_map_keys("channels.telegram")
        .expect("channels.telegram should be a map-keyed section");
    keys_after.sort();

    assert_eq!(
        keys_before, keys_after,
        "replace on an existing alias must not add or remove aliases"
    );
    assert!(!after.channels.telegram["existingbot"].enabled);
}

#[test]
fn config_patch_remove_and_test_do_not_materialize_unknown_alias() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    // Establish a config.toml on disk first via a benign, unrelated op.
    run_cli_patch_success(
        config_dir.path(),
        br#"[{"op":"replace","path":"/gateway/host","value":"127.0.0.4"}]"#,
    );

    let remove_envelope = run_cli_patch(
        config_dir.path(),
        br#"[{"op":"remove","path":"/channels/telegram/ghostbot/enabled"}]"#,
    );
    assert_eq!(remove_envelope["code"], "path_not_found");
    assert_eq!(
        remove_envelope["path"],
        "channels.telegram.ghostbot.enabled"
    );

    let test_envelope = run_cli_patch(
        config_dir.path(),
        br#"[{"op":"test","path":"/channels/telegram/ghostbot/enabled","value":true}]"#,
    );
    assert_eq!(test_envelope["code"], "path_not_found");
    assert_eq!(test_envelope["path"], "channels.telegram.ghostbot.enabled");

    let saved =
        std::fs::read_to_string(config_dir.path().join("config.toml")).expect("read saved config");
    let cfg: Config = toml::from_str(&saved).expect("saved config should parse");
    assert!(
        !cfg.channels.telegram.contains_key("ghostbot"),
        "remove/test must not materialize an unknown alias: {saved}"
    );
}

#[test]
fn config_patch_add_on_reserved_default_agent_is_refused() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    // Establish a config.toml on disk first via a benign, unrelated op.
    run_cli_patch_success(
        config_dir.path(),
        br#"[{"op":"replace","path":"/gateway/host","value":"127.0.0.5"}]"#,
    );

    let envelope = run_cli_patch(
        config_dir.path(),
        br#"[{"op":"add","path":"/agents/default/enabled","value":true}]"#,
    );

    assert_eq!(envelope["code"], "validation_failed");
    assert_eq!(envelope["path"], "agents.default.enabled");
    assert_eq!(envelope["op_index"], 0);
    assert!(
        envelope["message"]
            .as_str()
            .expect("message")
            .contains("alias `default` is reserved"),
        "message should name the reserved alias: {envelope}"
    );

    let saved =
        std::fs::read_to_string(config_dir.path().join("config.toml")).expect("read saved config");
    let cfg: Config = toml::from_str(&saved).expect("saved config should parse");
    assert!(
        !cfg.agents.contains_key("default"),
        "reserved alias must not be materialized"
    );
}

#[test]
fn config_patch_add_failure_after_materialization_does_not_persist_phantom_alias() {
    let config_dir = tempfile::tempdir().expect("temp config dir");

    let envelope = run_cli_patch(
        config_dir.path(),
        br#"[{"op":"add","path":"/channels/telegram/phantombot/reply_min_interval_secs","value":99999}]"#,
    );
    assert_eq!(envelope["code"], "invalid_numeric_range");
    assert_eq!(
        envelope["path"],
        "channels.telegram.phantombot.reply_min_interval_secs"
    );

    let saved =
        std::fs::read_to_string(config_dir.path().join("config.toml")).expect("read saved config");
    let cfg: Config = toml::from_str(&saved).expect("saved config should parse");
    assert!(
        !cfg.channels.telegram.contains_key("phantombot"),
        "a failed op must not leave a phantom alias on disk: {saved}"
    );
}
