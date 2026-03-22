//! A2A (Agent-to-Agent) protocol server handlers.
//!
//! Serves the agent card at `GET /.well-known/agent-card.json` and processes
//! inbound JSON-RPC 2.0 task requests at `POST /a2a`.

use super::AppState;
use crate::security::pairing::constant_time_eq;
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Types ────────────────────────────────────────────────────────

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
    let (Some(_card), Some(task_store)) = (&state.a2a_agent_card, &state.a2a_task_store)
    else {
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

    // Store task as working
    {
        let mut tasks = task_store.tasks.write().await;
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
    let session_id = format!("a2a-{task_id}");
    match Box::pin(crate::agent::process_message(
        config,
        &message,
        Some(&session_id),
    ))
    .await
    {
        Ok(response) => {
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
            Json(rpc_error(
                req.id,
                -32001,
                &format!("Task not found: {task_id}"),
            )),
        ),
    }
}

// ── Helpers ──────────────────────────────────────────────────────

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
}
