//! # A2A Protocol — MVP Implementation
//!
//! Implements a minimal subset of the A2A (Agent-to-Agent) protocol:
//! - Agent Card discovery (`GET /.well-known/agent-card.json`)
//! - `message/send` (synchronous request/response, no async queue)
//! - `tasks/get` (polling only)
//! - Bearer token authentication
//!
//! **Not yet implemented (see issue #3566):**
//! - `message/stream` (SSE)
//! - `tasks/cancel`
//! - `input-required` state / multi-turn conversations (`contextId`)
//! - Push notifications
//! - Structured/binary message parts (`data`, `raw`)
//! - Async task execution
//! - Task persistence

use super::AppState;
use crate::security::pairing::constant_time_eq;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Types ────────────────────────────────────────────────────────

/// Maximum number of in-flight tasks to prevent memory exhaustion.
const MAX_TASKS: usize = 10_000;

/// In-memory store for A2A task state.
pub struct TaskStore {
    tasks: RwLock<HashMap<String, TaskState>>,
}

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
        }
    }
}

/// State of an inbound A2A task.
#[derive(Debug, Clone, Serialize)]
pub struct TaskState {
    pub id: String,
    pub status: TaskStatus,
    pub artifacts: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Submitted,
    Working,
    Completed,
    Failed,
}

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

// ── Agent card generation ────────────────────────────────────────

/// Generate the A2A agent card from configuration.
pub fn generate_agent_card(config: &crate::config::Config) -> serde_json::Value {
    let a2a = &config.a2a;

    let name = a2a
        .agent_name
        .clone()
        .unwrap_or_else(|| "ZeroClaw Agent".to_string());

    let description = a2a
        .description
        .clone()
        .unwrap_or_else(|| "ZeroClaw autonomous agent".to_string());

    let version = a2a
        .version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    let base_url = a2a
        .public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}:{}", config.gateway.host, config.gateway.port));

    let skills: Vec<serde_json::Value> = if a2a.capabilities.is_empty() {
        vec![json!({
            "id": "general",
            "name": "General",
            "description": "General-purpose autonomous agent",
            "tags": ["general"],
            "examples": ["Help me with a task"]
        })]
    } else {
        a2a.capabilities
            .iter()
            .map(|c| {
                json!({
                    "id": c,
                    "name": c,
                    "description": format!("{c} capability"),
                    "tags": [c],
                    "examples": []
                })
            })
            .collect()
    };

    json!({
        "name": name,
        "description": description,
        "version": version,
        "url": base_url,
        "capabilities": {
            "streaming": false,
            "pushNotifications": false
        },
        "defaultInputModes": ["text"],
        "defaultOutputModes": ["text"],
        "skills": skills,
        "provider": {
            "organization": "ZeroClaw"
        },
        "authentication": {
            "schemes": ["bearer"]
        }
    })
}

// ── Handlers ─────────────────────────────────────────────────────

/// `GET /.well-known/agent-card.json` — unauthenticated discovery endpoint.
pub async fn handle_agent_card(State(state): State<AppState>) -> impl IntoResponse {
    match &state.a2a_agent_card {
        Some(card) => (StatusCode::OK, Json(card.as_ref().clone())).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "A2A protocol not enabled"})),
        )
            .into_response(),
    }
}

/// `POST /a2a` — authenticated JSON-RPC 2.0 task endpoint.
pub async fn handle_a2a_rpc(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    // Check feature enabled
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": "A2A protocol not enabled"}})),
        )
            .into_response();
    };

    // Authenticate
    if let Err(resp) = require_a2a_auth(&state, &headers) {
        return resp.into_response();
    }

    // Validate JSON-RPC version
    if body.jsonrpc != "2.0" {
        return (
            StatusCode::BAD_REQUEST,
            Json(rpc_error(body.id, -32600, "Invalid JSON-RPC version")),
        )
            .into_response();
    }

    match body.method.as_str() {
        "message/send" => Box::pin(handle_message_send(&state, task_store, body))
            .await
            .into_response(),
        "tasks/get" => handle_tasks_get(task_store, body).await.into_response(),
        _ => (
            StatusCode::OK,
            Json(rpc_error(
                body.id,
                -32601,
                &format!("Method not found: {}", body.method),
            )),
        )
            .into_response(),
    }
}

// ── Auth helper ──────────────────────────────────────────────────

fn require_a2a_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    // Extract bearer token from Authorization header
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .unwrap_or("");

    // Check dedicated A2A bearer token first
    {
        let config = state.config.lock();
        if let Some(ref a2a_token) = config.a2a.bearer_token {
            if !a2a_token.is_empty() {
                return if constant_time_eq(token, a2a_token) {
                    Ok(())
                } else {
                    Err((
                        StatusCode::UNAUTHORIZED,
                        Json(
                            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": "Unauthorized"}}),
                        ),
                    ))
                };
            }
        }
    }

    // Fall back to gateway pairing auth
    if !state.pairing.require_pairing() {
        return Ok(());
    }

    if state.pairing.is_authenticated(token) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": "Unauthorized"}}),
            ),
        ))
    }
}

// ── Method handlers ──────────────────────────────────────────────

async fn handle_message_send(
    state: &AppState,
    task_store: &Arc<TaskStore>,
    req: JsonRpcRequest,
) -> (StatusCode, Json<serde_json::Value>) {
    // Extract message text from params
    let message = req
        .params
        .pointer("/message/parts")
        .and_then(|parts| parts.as_array())
        .and_then(|parts| {
            parts.iter().find_map(|p| {
                if p.get("kind").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None
                }
            })
        })
        .or_else(|| {
            // Simple text fallback
            req.params
                .get("message")
                .and_then(|m| m.as_str())
                .map(String::from)
        });

    let Some(message) = message else {
        return (
            StatusCode::OK,
            Json(rpc_error(
                req.id,
                -32602,
                "Invalid params: missing message text",
            )),
        );
    };

    let task_id = uuid::Uuid::new_v4().to_string();

    // Store task as working (enforce capacity limit to prevent memory exhaustion)
    {
        let mut tasks = task_store.tasks.write().await;
        if tasks.len() >= MAX_TASKS {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(rpc_error(
                    req.id,
                    -32000,
                    "Task store full — too many in-flight tasks",
                )),
            );
        }
        tasks.insert(
            task_id.clone(),
            TaskState {
                id: task_id.clone(),
                status: TaskStatus::Working,
                artifacts: vec![],
            },
        );
    }

    // Process via agent pipeline
    let config = state.config.lock().clone();
    let telegram_notify = config.a2a.notify_chat_id.and_then(|chat_id| {
        config
            .channels_config
            .telegram
            .as_ref()
            .map(|t| (t.bot_token.clone(), chat_id))
    });
    let session_id = format!("a2a-{task_id}");
    match Box::pin(crate::agent::process_message(
        config,
        &message,
        Some(&session_id),
    ))
    .await
    {
        Ok(response) => {
            // Notify Telegram group about A2A activity
            if let Some((ref token, chat_id)) = telegram_notify {
                let notice = format!(
                    "\u{1f4e8} *A2A received:* _{}_\n\n{}",
                    message.replace('*', "\\*").replace('_', "\\_"),
                    response
                );
                notify_telegram_chat(token, chat_id, &notice).await;
            }

            let artifact = json!({
                "artifactId": uuid::Uuid::new_v4().to_string(),
                "name": "response",
                "parts": [{ "kind": "text", "text": response }]
            });
            let mut tasks = task_store.tasks.write().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = TaskStatus::Completed;
                task.artifacts = vec![artifact.clone()];
            }

            (
                StatusCode::OK,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": {
                        "id": task_id,
                        "status": { "state": "completed" },
                        "artifacts": [artifact]
                    }
                })),
            )
        }
        Err(e) => {
            tracing::error!(task_id = %task_id, error = %e, "A2A task processing failed");
            let mut tasks = task_store.tasks.write().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = TaskStatus::Failed;
            }

            (
                StatusCode::OK,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": {
                        "id": task_id,
                        "status": { "state": "failed", "message": "Internal processing error" }
                    }
                })),
            )
        }
    }
}

async fn handle_tasks_get(
    task_store: &Arc<TaskStore>,
    req: JsonRpcRequest,
) -> (StatusCode, Json<serde_json::Value>) {
    let task_id = req.params.get("id").and_then(|v| v.as_str()).unwrap_or("");

    if task_id.is_empty() {
        return (
            StatusCode::OK,
            Json(rpc_error(req.id, -32602, "Invalid params: missing task id")),
        );
    }

    let tasks = task_store.tasks.read().await;
    match tasks.get(task_id) {
        Some(task) => (
            StatusCode::OK,
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "result": {
                    "id": task.id,
                    "status": { "state": task.status },
                    "artifacts": task.artifacts
                }
            })),
        ),
        None => (
            StatusCode::OK,
            Json(rpc_error(req.id, -32001, "Task not found")),
        ),
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Best-effort Telegram notification for A2A activity.
/// Sends a message to a known chat ID (e.g. a group chat).
async fn notify_telegram_chat(bot_token: &str, chat_id: i64, text: &str) {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let _ = client
        .post(&url)
        .json(&json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "Markdown"
        }))
        .send()
        .await;
}

fn rpc_error(id: serde_json::Value, code: i32, message: &str) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{AppState, GatewayRateLimiter, IdempotencyStore, nodes};
    use crate::memory::{Memory, MemoryCategory, MemoryEntry};
    use crate::providers::Provider;
    use crate::security::pairing::PairingGuard;
    use async_trait::async_trait;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    use parking_lot::Mutex;
    use std::time::Duration;

    // ── Test mocks ───────────────────────────────────────────

    struct MockMemory;

    #[async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }

    /// Build an `AppState` with optional A2A and pairing configuration.
    fn a2a_test_state(
        bearer_token: Option<&str>,
        require_pairing: bool,
        paired_tokens: &[String],
    ) -> AppState {
        let mut config = crate::config::Config::default();
        config.a2a.enabled = true;
        if let Some(token) = bearer_token {
            config.a2a.bearer_token = Some(token.to_string());
        }

        let card = generate_agent_card(&config);

        AppState {
            config: Arc::new(Mutex::new(config)),
            provider: Arc::new(MockProvider),
            model: "test-model".into(),
            temperature: 0.0,
            mem: Arc::new(MockMemory),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(require_pairing, paired_tokens)),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100)),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            whatsapp: None,
            whatsapp_app_secret: None,
            linq: None,
            linq_signing_secret: None,
            nextcloud_talk: None,
            nextcloud_talk_webhook_secret: None,
            wati: None,
            gmail_push: None,
            observer: Arc::new(crate::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            session_backend: None,
            device_registry: None,
            pending_pairings: None,
            path_prefix: String::new(),
            canvas_store: crate::tools::canvas::CanvasStore::new(),
            a2a_agent_card: Some(Arc::new(card)),
            a2a_task_store: Some(Arc::new(TaskStore::new())),
            auth_limiter: Arc::new(crate::gateway::auth_rate_limit::AuthRateLimiter::new()),
            session_queue: Arc::new(crate::gateway::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
        }
    }

    fn bearer_header(token: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    async fn response_json(resp: axum::response::Response) -> serde_json::Value {
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    // ── Unit tests ───────────────────────────────────────────

    #[test]
    fn agent_card_generation_defaults() {
        let config = crate::config::Config {
            a2a: crate::config::A2aConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let card = generate_agent_card(&config);
        assert_eq!(card["name"], "ZeroClaw Agent");
        assert!(card["url"].as_str().unwrap().starts_with("http://"));
        assert!(card["capabilities"].is_object());
        assert_eq!(card["capabilities"]["streaming"], false);
        assert!(card["authentication"]["schemes"].is_array());
        // Skills should have proper AgentSkill structure
        let skills = card["skills"].as_array().unwrap();
        assert!(!skills.is_empty());
        assert!(skills[0]["id"].is_string());
        assert!(skills[0]["name"].is_string());
        assert!(skills[0]["description"].is_string());
    }

    #[test]
    fn agent_card_generation_custom() {
        let config = crate::config::Config {
            a2a: crate::config::A2aConfig {
                enabled: true,
                agent_name: Some("my-agent".into()),
                description: Some("My custom agent".into()),
                public_url: Some("https://agent.example.com".into()),
                capabilities: vec!["search".into(), "code".into()],
                ..Default::default()
            },
            ..Default::default()
        };

        let card = generate_agent_card(&config);
        assert_eq!(card["name"], "my-agent");
        assert_eq!(card["description"], "My custom agent");
        assert_eq!(card["url"], "https://agent.example.com");
        assert_eq!(card["skills"].as_array().unwrap().len(), 2);
        assert_eq!(card["skills"][0]["id"], "search");
    }

    #[test]
    fn rpc_error_format() {
        let err = rpc_error(json!(1), -32600, "Test error");
        assert_eq!(err["jsonrpc"], "2.0");
        assert_eq!(err["id"], 1);
        assert_eq!(err["error"]["code"], -32600);
        assert_eq!(err["error"]["message"], "Test error");
    }

    #[tokio::test]
    async fn task_store_lifecycle() {
        let store = TaskStore::new();
        let task_id = "test-123".to_string();

        // Insert
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                task_id.clone(),
                TaskState {
                    id: task_id.clone(),
                    status: TaskStatus::Working,
                    artifacts: vec![],
                },
            );
        }

        // Read
        {
            let tasks = store.tasks.read().await;
            let task = tasks.get(&task_id).unwrap();
            assert_eq!(task.status, TaskStatus::Working);
        }

        // Update
        {
            let mut tasks = store.tasks.write().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = TaskStatus::Completed;
                task.artifacts = vec![json!({"text": "done"})];
            }
        }

        // Verify
        {
            let tasks = store.tasks.read().await;
            let task = tasks.get(&task_id).unwrap();
            assert_eq!(task.status, TaskStatus::Completed);
            assert_eq!(task.artifacts.len(), 1);
        }
    }

    #[test]
    fn max_tasks_constant_is_reasonable() {
        let max = MAX_TASKS;
        assert!(max >= 1_000, "MAX_TASKS should allow reasonable load");
        assert!(max <= 100_000, "MAX_TASKS should cap memory growth");
    }

    // ── Handler integration tests ────────────────────────────

    #[tokio::test]
    async fn agent_card_endpoint_returns_card_when_enabled() {
        let state = a2a_test_state(Some("secret"), false, &[]);
        let resp = handle_agent_card(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        assert_eq!(body["name"], "ZeroClaw Agent");
        assert!(body["skills"].is_array());
    }

    #[tokio::test]
    async fn agent_card_endpoint_returns_404_when_disabled() {
        let mut state = a2a_test_state(None, false, &[]);
        state.a2a_agent_card = None;
        let resp = handle_agent_card(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rpc_rejects_missing_bearer_when_token_configured() {
        let state = a2a_test_state(Some("my-secret"), false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "x"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rpc_rejects_wrong_bearer_token() {
        let state = a2a_test_state(Some("correct"), false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "x"}),
        };
        let headers = bearer_header("wrong");
        let resp = handle_a2a_rpc(State(state), headers, Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rpc_accepts_correct_bearer_token() {
        let state = a2a_test_state(Some("correct"), false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "nonexistent"}),
        };
        let headers = bearer_header("correct");
        let resp = handle_a2a_rpc(State(state), headers, Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        // Should get "task not found" — not an auth error
        assert_eq!(body["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn rpc_allows_unauthenticated_when_no_auth_configured() {
        let state = a2a_test_state(None, false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "x"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        // Should reach method handler, not auth rejection
        assert_eq!(body["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn rpc_rejects_unauthenticated_when_pairing_required() {
        let state = a2a_test_state(None, true, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "x"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rpc_rejects_invalid_jsonrpc_version() {
        let state = a2a_test_state(None, false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "1.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rpc_unknown_method_returns_method_not_found() {
        let state = a2a_test_state(None, false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(42),
            method: "tasks/cancel".into(),
            params: json!({}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        let body = response_json(resp).await;
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn tasks_get_returns_not_found_for_missing_task() {
        let store = Arc::new(TaskStore::new());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "no-such-task"}),
        };
        let (status, Json(body)) = handle_tasks_get(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32001);
        // Error message must NOT echo the user-supplied task ID
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(
            !msg.contains("no-such-task"),
            "error must not echo user input"
        );
    }

    #[tokio::test]
    async fn tasks_get_returns_task_when_exists() {
        let store = Arc::new(TaskStore::new());
        {
            let mut tasks = store.tasks.write().await;
            tasks.insert(
                "task-abc".into(),
                TaskState {
                    id: "task-abc".into(),
                    status: TaskStatus::Completed,
                    artifacts: vec![json!({"text": "result"})],
                },
            );
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({"id": "task-abc"}),
        };
        let (status, Json(body)) = handle_tasks_get(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["error"].is_null());
        assert_eq!(body["result"]["id"], "task-abc");
        assert_eq!(body["result"]["status"]["state"], "completed");
        assert_eq!(body["result"]["artifacts"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn tasks_get_rejects_empty_task_id() {
        let store = Arc::new(TaskStore::new());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "tasks/get".into(),
            params: json!({}),
        };
        let (status, Json(body)) = handle_tasks_get(&store, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn task_store_capacity_limit_enforced() {
        let store = TaskStore::new();
        {
            let mut tasks = store.tasks.write().await;
            for i in 0..MAX_TASKS {
                tasks.insert(
                    format!("task-{i}"),
                    TaskState {
                        id: format!("task-{i}"),
                        status: TaskStatus::Completed,
                        artifacts: vec![],
                    },
                );
            }
            assert_eq!(tasks.len(), MAX_TASKS);
        }

        // Verify the store is at capacity — direct insert would exceed
        {
            let tasks = store.tasks.read().await;
            assert_eq!(tasks.len(), MAX_TASKS);
        }
    }

    #[tokio::test]
    async fn rpc_disabled_returns_404() {
        let mut state = a2a_test_state(None, false, &[]);
        state.a2a_agent_card = None;
        state.a2a_task_store = None;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "message/send".into(),
            params: json!({"message": {"parts": [{"kind": "text", "text": "hello"}]}}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Gap 1: message/send handler (error path) ─────────────

    #[tokio::test]
    async fn message_send_missing_text_returns_invalid_params() {
        let state = a2a_test_state(None, false, &[]);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "message/send".into(),
            params: json!({}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        let body = response_json(resp).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("missing message")
        );
    }

    #[tokio::test]
    async fn message_send_accepts_simple_text_fallback() {
        // Tests the simple `params.message` string fallback path.
        // process_message will fail (no provider configured), so we
        // verify the task is created and the failure is handled cleanly.
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.clone().unwrap();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(99),
            method: "message/send".into(),
            params: json!({"message": "hello from simple fallback"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        // process_message fails in test (no provider) → task should be "failed"
        let result = &body["result"];
        assert!(result["id"].is_string());
        assert_eq!(result["status"]["state"], "failed");

        // Verify the task was stored with Failed status
        let task_id = result["id"].as_str().unwrap();
        let tasks = task_store.tasks.read().await;
        let task = tasks.get(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
    }

    #[tokio::test]
    async fn message_send_accepts_parts_format() {
        // Tests the A2A-standard message/parts format.
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.clone().unwrap();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(100),
            method: "message/send".into(),
            params: json!({
                "message": {
                    "role": "user",
                    "parts": [{"kind": "text", "text": "structured message"}],
                    "messageId": "msg-001"
                }
            }),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = response_json(resp).await;
        let result = &body["result"];
        assert!(result["id"].is_string());
        // Will fail due to no provider, but verifies the message was extracted
        assert_eq!(result["status"]["state"], "failed");

        // Task was created in the store
        let task_id = result["id"].as_str().unwrap();
        let tasks = task_store.tasks.read().await;
        assert!(tasks.contains_key(task_id));
    }

    #[tokio::test]
    async fn message_send_rejects_when_store_full() {
        let state = a2a_test_state(None, false, &[]);
        let task_store = state.a2a_task_store.clone().unwrap();

        // Fill the store to capacity
        {
            let mut tasks = task_store.tasks.write().await;
            for i in 0..MAX_TASKS {
                tasks.insert(
                    format!("fill-{i}"),
                    TaskState {
                        id: format!("fill-{i}"),
                        status: TaskStatus::Completed,
                        artifacts: vec![],
                    },
                );
            }
        }

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(1),
            method: "message/send".into(),
            params: json!({"message": "should be rejected"}),
        };
        let resp = handle_a2a_rpc(State(state), HeaderMap::new(), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_json(resp).await;
        assert_eq!(body["error"]["code"], -32000);
        assert!(body["error"]["message"].as_str().unwrap().contains("full"));
    }
}
