//! ACP-over-HTTP server endpoint.
//!
//! Implements the Agent Communication Protocol (ACP) JSON-RPC 2.0 over HTTP
//! with Server-Sent Events (SSE) streaming. This allows another zeroclaw
//! instance (or any ACP client) to delegate tasks and receive streamed results.
//!
//! Protocol lifecycle:
//!   POST /acp  method=initialize       → Acp-Session-Id header + server info
//!   POST /acp  method=session/new      → creates agent session, returns sessionId
//!   POST /acp  method=session/prompt   → runs agent loop, streams via SSE
//!   DELETE /acp                         → tears down transport session

use crate::config::Config;
use crate::gateway::AppState;
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio_stream::StreamExt;
use uuid::Uuid;

// ── Types ────────────────────────────────────────────────────────

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    pub method: String,
    pub id: serde_json::Value,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// `initialize` params from the client.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    #[allow(dead_code)]
    protocol_version: Option<String>,
    #[allow(dead_code)]
    capabilities: Option<serde_json::Value>,
    #[allow(dead_code)]
    client_info: Option<serde_json::Value>,
}

/// `session/new` params.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionNewParams {
    #[allow(dead_code)]
    cwd: Option<String>,
    #[allow(dead_code)]
    mcp_servers: Option<Vec<serde_json::Value>>,
}

/// `session/prompt` params.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionPromptParams {
    session_id: String,
    prompt: Vec<PromptContent>,
}

#[derive(Debug, Deserialize)]
struct PromptContent {
    #[allow(dead_code)]
    r#type: Option<String>,
    text: Option<String>,
}

// ── Transport session store ──────────────────────────────────────

/// A single ACP transport session (created by `initialize`).
#[derive(Debug, Clone)]
pub struct AcpTransportSession {
    pub id: String,
    /// Agent session ID (set by `session/new`), used as conversation history key.
    pub agent_session_id: Option<String>,
    /// Accumulated conversation history across `session/prompt` calls.
    pub history: Vec<crate::providers::ChatMessage>,
    pub created_at: Instant,
}

/// Thread-safe store of active ACP transport sessions.
#[derive(Debug, Clone)]
pub struct AcpSessionStore {
    sessions: Arc<Mutex<HashMap<String, AcpTransportSession>>>,
    /// Maximum session age before automatic eviction.
    ttl: std::time::Duration,
}

impl AcpSessionStore {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            ttl: std::time::Duration::from_secs(ttl_secs),
        }
    }

    /// Create a new transport session. Returns the session ID.
    pub fn create(&self) -> String {
        let id = Uuid::new_v4().to_string();
        let session = AcpTransportSession {
            id: id.clone(),
            agent_session_id: None,
            history: Vec::new(),
            created_at: Instant::now(),
        };
        let mut sessions = self.sessions.lock();
        self.evict_expired(&mut sessions);
        sessions.insert(id.clone(), session);
        id
    }

    /// Look up a transport session by ID.
    pub fn get(&self, id: &str) -> Option<AcpTransportSession> {
        let sessions = self.sessions.lock();
        sessions.get(id).filter(|s| s.created_at.elapsed() < self.ttl).cloned()
    }

    /// Update a session in place.
    pub fn update(&self, session: AcpTransportSession) {
        let mut sessions = self.sessions.lock();
        sessions.insert(session.id.clone(), session);
    }

    /// Remove a transport session.
    pub fn remove(&self, id: &str) -> bool {
        self.sessions.lock().remove(id).is_some()
    }

    /// Evict sessions older than TTL.
    fn evict_expired(&self, sessions: &mut HashMap<String, AcpTransportSession>) {
        sessions.retain(|_, s| s.created_at.elapsed() < self.ttl);
    }

    /// Number of active sessions (for testing/metrics).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.sessions.lock().len()
    }
}

// ── JSON-RPC response helpers ────────────────────────────────────

fn jsonrpc_result(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn jsonrpc_error(
    id: &serde_json::Value,
    code: i64,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn jsonrpc_notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
}

/// Wrap a JSON-RPC result as an SSE `data:` line.
fn sse_line(value: &serde_json::Value) -> String {
    format!("data: {}\n\n", serde_json::to_string(value).unwrap_or_default())
}

// ── Handlers ─────────────────────────────────────────────────────

/// Main `POST /acp` handler — dispatches by JSON-RPC `method`.
pub async fn handle_acp(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Auth check — reuse gateway pairing
    if state.pairing.require_pairing() {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Unauthorized — pair first via POST /pair"})),
            )
                .into_response();
        }
    }

    // Rate limit
    let rate_key = super::client_key_from_request(
        Some(peer_addr),
        &headers,
        state.trust_forwarded_headers,
    );
    if !state.rate_limiter.allow_webhook(&rate_key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "Rate limited", "retry_after": 60})),
        )
            .into_response();
    }

    // Parse JSON-RPC request
    let req: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(jsonrpc_error(
                    &serde_json::Value::Null,
                    -32700,
                    &format!("Parse error: {e}"),
                )),
            )
                .into_response();
        }
    };

    let acp_store = match state.acp_sessions.as_ref() {
        Some(store) => store,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "ACP server not enabled"})),
            )
                .into_response();
        }
    };

    match req.method.as_str() {
        "initialize" => handle_initialize(&req, acp_store).into_response(),
        "session/new" => handle_session_new(&req, &headers, acp_store).into_response(),
        "session/prompt" => {
            handle_session_prompt(&req, &headers, acp_store, &state).await
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(jsonrpc_error(&req.id, -32601, &format!("Unknown method: {}", req.method))),
        )
            .into_response(),
    }
}

/// `DELETE /acp` — tear down a transport session.
pub async fn handle_acp_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let acp_store = match state.acp_sessions.as_ref() {
        Some(store) => store,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "ACP server not enabled"})),
            )
                .into_response();
        }
    };

    let session_id = headers
        .get("Acp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if session_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Missing Acp-Session-Id header"})),
        )
            .into_response();
    }

    if acp_store.remove(session_id) {
        tracing::info!(acp_session_id = session_id, "ACP session deleted");
        (StatusCode::OK, Json(serde_json::json!({"deleted": true}))).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session not found"})),
        )
            .into_response()
    }
}

// ── Method handlers ──────────────────────────────────────────────

/// `initialize` — create transport session, return Acp-Session-Id header.
fn handle_initialize(req: &JsonRpcRequest, store: &AcpSessionStore) -> Response {
    let transport_id = store.create();
    tracing::info!(acp_session_id = %transport_id, "ACP transport session initialized");

    let result = jsonrpc_result(
        &req.id,
        serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "serverInfo": {
                "name": "zeroclaw-acp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    );

    // Return as SSE (the client reads SSE even for initialize)
    let body = sse_line(&result);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("Acp-Session-Id", &transport_id)
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `session/new` — create agent session within transport session.
fn handle_session_new(
    req: &JsonRpcRequest,
    headers: &HeaderMap,
    store: &AcpSessionStore,
) -> Response {
    let transport_id = headers
        .get("Acp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let mut session = match store.get(transport_id) {
        Some(s) => s,
        None => {
            let err = jsonrpc_error(&req.id, -32000, "Invalid or expired Acp-Session-Id");
            let body = sse_line(&err);
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };

    let agent_session_id = format!("acp:{}", Uuid::new_v4());
    session.agent_session_id = Some(agent_session_id.clone());
    store.update(session);

    tracing::info!(
        acp_session_id = transport_id,
        agent_session_id = %agent_session_id,
        "ACP agent session created"
    );

    let result = jsonrpc_result(
        &req.id,
        serde_json::json!({ "sessionId": agent_session_id }),
    );
    let body = sse_line(&result);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `session/prompt` — run the full agent tool loop and stream results as SSE.
async fn handle_session_prompt(
    req: &JsonRpcRequest,
    headers: &HeaderMap,
    store: &AcpSessionStore,
    state: &AppState,
) -> Response {
    // Validate transport session
    let transport_id = headers
        .get("Acp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let session = match store.get(&transport_id) {
        Some(s) => s,
        None => {
            let err = jsonrpc_error(&req.id, -32000, "Invalid or expired Acp-Session-Id");
            return sse_response(sse_line(&err));
        }
    };

    let agent_session_id = match &session.agent_session_id {
        Some(id) => id.clone(),
        None => {
            let err = jsonrpc_error(&req.id, -32000, "No agent session — call session/new first");
            return sse_response(sse_line(&err));
        }
    };

    // Parse prompt params
    let params: SessionPromptParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            let err = jsonrpc_error(&req.id, -32602, &format!("Invalid params: {e}"));
            return sse_response(sse_line(&err));
        }
    };

    // Validate session ID matches
    if params.session_id != agent_session_id {
        let err = jsonrpc_error(&req.id, -32000, "sessionId mismatch");
        return sse_response(sse_line(&err));
    }

    // Extract prompt text
    let prompt_text = params
        .prompt
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    if prompt_text.is_empty() {
        let err = jsonrpc_error(&req.id, -32602, "Empty prompt");
        return sse_response(sse_line(&err));
    }

    let request_id = req.id.clone();

    tracing::info!(
        acp_session_id = %transport_id,
        agent_session_id = %agent_session_id,
        prompt_len = prompt_text.len(),
        history_len = session.history.len(),
        "ACP session/prompt starting"
    );

    // Clone what we need for the async task
    let config = state.config.lock().clone();
    let existing_history = session.history.clone();
    let store_clone = store.clone();

    // Create a channel for streaming SSE events
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(256);

    // Spawn the agent loop in a background task
    tokio::spawn(async move {
        let result = run_acp_agent_loop(
            config,
            &prompt_text,
            existing_history,
            tx.clone(),
        )
        .await;

        match result {
            Ok((response_text, updated_history)) => {
                // Persist updated history back to the session store
                if let Some(mut session) = store_clone.get(&transport_id) {
                    session.history = updated_history;
                    store_clone.update(session);
                }

                // Send final result
                let result_msg = jsonrpc_result(
                    &request_id,
                    serde_json::json!({
                        "content": [{ "type": "text", "text": response_text }]
                    }),
                );
                let _ = tx.send(sse_line(&result_msg)).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "ACP agent loop failed");
                let err = jsonrpc_error(&request_id, -32000, &format!("Agent error: {e}"));
                let _ = tx.send(sse_line(&err)).await;
            }
        }
    });

    // Convert the receiver into an SSE stream
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body_stream = stream.map(|chunk| Ok::<_, std::io::Error>(axum::body::Bytes::from(chunk)));

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn sse_response(body: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Run the agent loop, delegating to `process_message_with_history`.
async fn run_acp_agent_loop(
    config: Config,
    message: &str,
    existing_history: Vec<crate::providers::ChatMessage>,
    tx: tokio::sync::mpsc::Sender<String>,
) -> anyhow::Result<(String, Vec<crate::providers::ChatMessage>)> {
    // Send a "started" notification
    let notif = jsonrpc_notification(
        "notifications/update",
        serde_json::json!({"update": {"content": {"text": "[Agent starting...]"}}}),
    );
    let _ = tx.send(sse_line(&notif)).await;

    crate::agent::process_message_with_history(config, message, existing_history).await
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_store_create_and_get() {
        let store = AcpSessionStore::new(3600);
        let id = store.create();
        assert!(!id.is_empty());
        let session = store.get(&id).expect("session should exist");
        assert_eq!(session.id, id);
        assert!(session.agent_session_id.is_none());
        assert!(session.history.is_empty());
    }

    #[test]
    fn session_store_update_persists() {
        let store = AcpSessionStore::new(3600);
        let id = store.create();
        let mut session = store.get(&id).unwrap();
        session.agent_session_id = Some("test-agent-123".to_string());
        store.update(session);
        let updated = store.get(&id).unwrap();
        assert_eq!(
            updated.agent_session_id.as_deref(),
            Some("test-agent-123")
        );
    }

    #[test]
    fn session_store_remove() {
        let store = AcpSessionStore::new(3600);
        let id = store.create();
        assert!(store.get(&id).is_some());
        assert!(store.remove(&id));
        assert!(store.get(&id).is_none());
        assert!(!store.remove(&id)); // second remove returns false
    }

    #[test]
    fn session_store_ttl_expiry() {
        let store = AcpSessionStore::new(0); // 0s TTL = expire immediately
        let id = store.create();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(store.get(&id).is_none());
    }

    #[test]
    fn session_store_nonexistent_get() {
        let store = AcpSessionStore::new(3600);
        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn jsonrpc_result_format() {
        let result = jsonrpc_result(
            &serde_json::json!(1),
            serde_json::json!({"sessionId": "test-123"}),
        );
        assert_eq!(result["jsonrpc"], "2.0");
        assert_eq!(result["id"], 1);
        assert_eq!(result["result"]["sessionId"], "test-123");
    }

    #[test]
    fn jsonrpc_error_format() {
        let err = jsonrpc_error(&serde_json::json!(2), -32600, "Invalid request");
        assert_eq!(err["jsonrpc"], "2.0");
        assert_eq!(err["id"], 2);
        assert_eq!(err["error"]["code"], -32600);
        assert_eq!(err["error"]["message"], "Invalid request");
    }

    #[test]
    fn sse_line_format() {
        let msg = serde_json::json!({"test": true});
        let line = sse_line(&msg);
        assert!(line.starts_with("data: "));
        assert!(line.ends_with("\n\n"));
        assert!(line.contains("\"test\":true"));
    }

    #[test]
    fn jsonrpc_request_parsing() {
        let json = r#"{"jsonrpc":"2.0","method":"initialize","id":1,"params":{"protocolVersion":"2025-03-26"}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, 1);
    }

    #[test]
    fn session_prompt_params_parsing() {
        let json = r#"{"sessionId":"acp:test","prompt":[{"type":"text","text":"hello"}]}"#;
        let params: SessionPromptParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.session_id, "acp:test");
        assert_eq!(params.prompt.len(), 1);
        assert_eq!(params.prompt[0].text.as_deref(), Some("hello"));
    }
}
