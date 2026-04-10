//! WebSocket agent chat handler.
//!
//! Connect: `ws://host:port/ws/chat?session_id=ID&name=My+Session`
//!
//! Protocol:
//! ```text
//! Server -> Client: {"type":"session_start","session_id":"...","name":"...","resumed":true,"message_count":42}
//! Client -> Server: {"type":"message","content":"Hello"}
//! Server -> Client: {"type":"chunk","content":"Hi! "}
//! Server -> Client: {"type":"tool_call","name":"shell","args":{...}}
//! Server -> Client: {"type":"tool_result","name":"shell","output":"..."}
//! Server -> Client: {"type":"done","full_response":"..."}
//! ```
//!
//! Query params:
//! - `session_id` — resume or create a session (default: new UUID)
//! - `name` — optional human-readable label for the session
//! - `token` — bearer auth token (alternative to Authorization header)

use super::AppState;
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, header},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tracing::debug;

/// Optional connection parameters sent as the first WebSocket message.
///
/// If the first message after upgrade is `{"type":"connect",...}`, these
/// parameters are extracted and an acknowledgement is sent back. Old clients
/// that send `{"type":"message",...}` as the first frame still work — the
/// message is processed normally (backward-compatible).
#[derive(Debug, Deserialize)]
struct ConnectParams {
    #[serde(rename = "type")]
    msg_type: String,
    /// Client-chosen session ID for memory persistence
    #[serde(default)]
    session_id: Option<String>,
    /// Device name for device registry tracking
    #[serde(default)]
    device_name: Option<String>,
    /// Client capabilities
    #[serde(default)]
    capabilities: Vec<String>,
}

/// The sub-protocol we support for the chat WebSocket.
const WS_PROTOCOL: &str = "zeroclaw.v1";

/// Prefix used in `Sec-WebSocket-Protocol` to carry a bearer token.
const BEARER_SUBPROTO_PREFIX: &str = "bearer.";

#[derive(Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
    pub session_id: Option<String>,
    /// Optional human-readable name for the session.
    pub name: Option<String>,
}

/// Extract a bearer token from WebSocket-compatible sources.
///
/// Precedence (first non-empty wins):
/// 1. `Authorization: Bearer <token>` header
/// 2. `Sec-WebSocket-Protocol: bearer.<token>` subprotocol
/// 3. `?token=<token>` query parameter
///
/// Browsers cannot set custom headers on `new WebSocket(url)`, so the query
/// parameter and subprotocol paths are required for browser-based clients.
fn extract_ws_token<'a>(headers: &'a HeaderMap, query_token: Option<&'a str>) -> Option<&'a str> {
    // 1. Authorization header
    if let Some(t) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
    {
        if !t.is_empty() {
            return Some(t);
        }
    }

    // 2. Sec-WebSocket-Protocol: bearer.<token>
    if let Some(t) = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|protos| {
            protos
                .split(',')
                .map(|p| p.trim())
                .find_map(|p| p.strip_prefix(BEARER_SUBPROTO_PREFIX))
        })
    {
        if !t.is_empty() {
            return Some(t);
        }
    }

    // 3. ?token= query parameter
    if let Some(t) = query_token {
        if !t.is_empty() {
            return Some(t);
        }
    }

    None
}

/// GET /ws/chat — WebSocket upgrade for agent chat
pub async fn handle_ws_chat(
    State(state): State<AppState>,
    Query(params): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Auth: check header, subprotocol, then query param (precedence order)
    if state.pairing.require_pairing() {
        let token = extract_ws_token(&headers, params.token.as_deref()).unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization header, Sec-WebSocket-Protocol bearer, or ?token= query param",
            )
                .into_response();
        }
    }

    // Echo Sec-WebSocket-Protocol if the client requests our sub-protocol.
    let ws = if headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map_or(false, |protos| {
            protos.split(',').any(|p| p.trim() == WS_PROTOCOL)
        }) {
        ws.protocols([WS_PROTOCOL])
    } else {
        ws
    };

    let session_id = params.session_id;
    let session_name = params.name;
    ws.on_upgrade(move |socket| handle_socket(socket, state, session_id, session_name))
        .into_response()
}

/// Gateway session key prefix to avoid collisions with channel sessions.
const GW_SESSION_PREFIX: &str = "gw_";

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    session_id: Option<String>,
    session_name: Option<String>,
) {
    let (mut sender, mut receiver) = socket.split();

    // Resolve session ID: use provided or generate a new UUID
    let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

    // Build a persistent Agent for this connection so history is maintained across turns.
    let config = state.config.lock().clone();
    let mut agent = match crate::agent::Agent::from_config(&config).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "Agent initialization failed");
            let err = serde_json::json!({
                "type": "error",
                "message": format!("Failed to initialise agent: {e}"),
                "code": "AGENT_INIT_FAILED"
            });
            let _ = sender.send(Message::Text(err.to_string().into())).await;
            let _ = sender
                .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: 1011,
                    reason: axum::extract::ws::Utf8Bytes::from_static(
                        "Agent initialization failed",
                    ),
                })))
                .await;
            return;
        }
    };
    agent.set_memory_session_id(Some(session_id.clone()));

    // Hydrate agent from persisted session (if available)
    let mut resumed = false;
    let mut message_count: usize = 0;
    let mut effective_name: Option<String> = None;
    if let Some(ref backend) = state.session_backend {
        let messages = backend.load(&session_key);
        if !messages.is_empty() {
            message_count = messages.len();
            agent.seed_history(&messages);
            resumed = true;
        }
        if let Some(ref name) = session_name {
            if !name.is_empty() {
                let _ = backend.set_session_name(&session_key, name);
                effective_name = Some(name.clone());
            }
        }
        if effective_name.is_none() {
            effective_name = backend.get_session_name(&session_key).unwrap_or(None);
        }
    }

    // Send session_start message to client
    let mut session_start = serde_json::json!({
        "type": "session_start",
        "session_id": session_id,
        "resumed": resumed,
        "message_count": message_count,
    });
    if let Some(ref name) = effective_name {
        session_start["name"] = serde_json::Value::String(name.clone());
    }
    let _ = sender
        .send(Message::Text(session_start.to_string().into()))
        .await;

    let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<Message>(64);
    let writer_handle = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            if sender.send(message).await.is_err() {
                break;
            }
        }
    });

    let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(64);

    // Optional connect handshake: if the first frame is `{"type":"connect",...}`
    // we acknowledge it and keep waiting for queued chat messages. If the first
    // frame is already a normal `{"type":"message",...}` payload, route it
    // through the same acceptance path used for all later steering messages.
    if let Some(first) = receiver.next().await {
        match first {
            Ok(Message::Text(text)) => {
                if let Some(connect_params) = parse_connect_frame(&text) {
                    debug!(
                        session_id = ?connect_params.session_id,
                        device_name = ?connect_params.device_name,
                        capabilities = ?connect_params.capabilities,
                        "WebSocket connect params received"
                    );
                    if let Some(sid) = &connect_params.session_id {
                        agent.set_memory_session_id(Some(sid.clone()));
                    }
                    let ack = serde_json::json!({
                        "type": "connected",
                        "message": "Connection established"
                    });
                    let _ = outbound_tx
                        .send(Message::Text(ack.to_string().into()))
                        .await;
                } else if accept_user_text_frame(&text, &steering_tx, &outbound_tx)
                    .await
                    .is_err()
                {
                    let _ = writer_handle.await;
                    return;
                }
            }
            Ok(Message::Close(_)) | Err(_) => {
                drop(outbound_tx);
                let _ = writer_handle.await;
                return;
            }
            _ => {}
        }
    }

    // Spawn a dedicated reader so inbound websocket frames keep flowing while
    // the coordinator is busy driving a steered turn.
    let reader_outbound_tx = outbound_tx.clone();
    let reader_steering_tx = steering_tx.clone();
    let reader_handle = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if accept_user_text_frame(&text, &reader_steering_tx, &reader_outbound_tx)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });
    drop(steering_tx);

    // Subscribe to the shared broadcast channel so cron/heartbeat events
    // and other gateway broadcasts are forwarded to this WebSocket client.
    let mut broadcast_rx = state.event_tx.subscribe();
    let broadcast_outbound_tx = outbound_tx.clone();
    let broadcast_handle = tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(event) => {
                    if broadcast_outbound_tx
                        .send(Message::Text(event.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // The per-session queue still serializes top-level turns only. Steering
    // happens inside one active turn through the connection-local inbox above.
    while let Some(content) = steering_rx.recv().await {
        let _session_guard = match state.session_queue.acquire(&session_key).await {
            Ok(guard) => guard,
            Err(error) => {
                let _ = outbound_tx.send(session_queue_error_message(&error)).await;
                continue;
            }
        };

        process_chat_turn_with_steering(
            &state,
            &mut agent,
            &outbound_tx,
            &content,
            &mut steering_rx,
            &session_key,
        )
        .await;
    }

    reader_handle.abort();
    broadcast_handle.abort();
    drop(outbound_tx);
    let _ = writer_handle.await;
}

/// Parse the optional first-frame websocket connect handshake.
fn parse_connect_frame(text: &str) -> Option<ConnectParams> {
    let connect = serde_json::from_str::<ConnectParams>(text).ok()?;
    (connect.msg_type == "connect").then_some(connect)
}

fn inbound_error(message: String, code: &str) -> Message {
    Message::Text(
        serde_json::json!({
            "type": "error",
            "message": message,
            "code": code,
        })
        .to_string()
        .into(),
    )
}

/// Validate websocket chat messages so the first fallback frame and later
/// steering frames share exactly one acceptance path.
fn parse_user_message_content(text: &str) -> Result<String, Message> {
    let parsed: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| inbound_error(format!("Invalid JSON: {e}"), "INVALID_JSON"))?;

    let msg_type = parsed["type"].as_str().unwrap_or("");
    if msg_type != "message" {
        return Err(inbound_error(
            format!(
                "Unsupported message type \"{msg_type}\". Send {{\"type\":\"message\",\"content\":\"your text\"}}"
            ),
            "UNKNOWN_MESSAGE_TYPE",
        ));
    }

    let content = parsed["content"].as_str().unwrap_or("").to_string();
    if content.trim().is_empty() {
        return Err(inbound_error(
            "Message content cannot be empty".to_string(),
            "EMPTY_CONTENT",
        ));
    }

    Ok(content)
}

async fn accept_user_text_frame(
    text: &str,
    steering_tx: &tokio::sync::mpsc::Sender<String>,
    outbound_tx: &tokio::sync::mpsc::Sender<Message>,
) -> Result<(), ()> {
    let content = match parse_user_message_content(text) {
        Ok(content) => content,
        Err(message) => {
            let _ = outbound_tx.send(message).await;
            return Ok(());
        }
    };

    match steering_tx.try_send(content) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            let _ = outbound_tx
                .send(inbound_error(
                    "Steering queue is full for this websocket connection".to_string(),
                    "STEERING_QUEUE_FULL",
                ))
                .await;
            Ok(())
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(()),
    }
}

/// Flatten all user text consumed during one admitted turn into the same
/// lossy plain-text shape used by other consolidation callers. This is not a
/// machine-recoverable message boundary encoding.
fn flatten_consolidation_input(consumed_messages: &[String]) -> String {
    consumed_messages.join("\n\n")
}

fn persist_turn_transcript(
    session_backend: Option<&std::sync::Arc<dyn crate::channels::session_backend::SessionBackend>>,
    session_key: &str,
    consumed_messages: &[String],
    assistant_response: Option<&str>,
) {
    let Some(backend) = session_backend else {
        return;
    };

    for message in consumed_messages {
        let user_msg = crate::providers::ChatMessage::user(message);
        let _ = backend.append(session_key, &user_msg);
    }

    if let Some(response) = assistant_response.filter(|response| !response.is_empty()) {
        let assistant_msg = crate::providers::ChatMessage::assistant(response);
        let _ = backend.append(session_key, &assistant_msg);
    }
}

fn session_queue_error_message(
    error: &crate::gateway::session_queue::SessionQueueError,
) -> Message {
    match error {
        crate::gateway::session_queue::SessionQueueError::QueueFull { .. } => inbound_error(
            "Session already has the maximum number of queued turns".to_string(),
            "SESSION_QUEUE_FULL",
        ),
        crate::gateway::session_queue::SessionQueueError::Timeout { .. } => inbound_error(
            "Timed out waiting for the active session turn to finish".to_string(),
            "SESSION_QUEUE_TIMEOUT",
        ),
    }
}

/// Process one top-level websocket turn while continuing to absorb queued
/// steering messages at the agent's safe drain boundaries.
async fn process_chat_turn_with_steering(
    state: &AppState,
    agent: &mut crate::agent::Agent,
    outbound_tx: &tokio::sync::mpsc::Sender<Message>,
    content: &str,
    steering_rx: &mut tokio::sync::mpsc::Receiver<String>,
    session_key: &str,
) {
    use crate::agent::TurnEvent;

    let provider_label = state
        .config
        .lock()
        .default_provider
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let _ = state.event_tx.send(serde_json::json!({
        "type": "agent_start",
        "provider": provider_label,
        "model": state.model,
    }));

    let turn_id = uuid::Uuid::new_v4().to_string();
    if let Some(ref backend) = state.session_backend {
        let _ = backend.set_session_state(session_key, "running", Some(&turn_id));
    }

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let content_owned = content.to_string();
    let turn_fut = async {
        agent
            .turn_streamed_with_steering_state(&content_owned, event_tx, Some(steering_rx))
            .await
    };
    let outbound_tx_clone = outbound_tx.clone();
    let forward_fut = async move {
        while let Some(event) = event_rx.recv().await {
            let ws_msg = match event {
                TurnEvent::Chunk { delta } => {
                    serde_json::json!({ "type": "chunk", "content": delta })
                }
                TurnEvent::Thinking { delta } => {
                    serde_json::json!({ "type": "thinking", "content": delta })
                }
                TurnEvent::ToolCall { name, args } => {
                    serde_json::json!({ "type": "tool_call", "name": name, "args": args })
                }
                TurnEvent::ToolResult { name, output } => {
                    serde_json::json!({ "type": "tool_result", "name": name, "output": output })
                }
            };
            if outbound_tx_clone
                .send(Message::Text(ws_msg.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    };

    let (result, ()) = tokio::join!(turn_fut, forward_fut);

    match result {
        Ok(outcome) => {
            let response = outcome.response;
            let consumed_messages = outcome.consumed_messages;

            persist_turn_transcript(
                state.session_backend.as_ref(),
                session_key,
                &consumed_messages,
                Some(&response),
            );

            if state.auto_save {
                let mem = state.mem.clone();
                let provider = state.provider.clone();
                let model = state.model.clone();
                let user_msg = flatten_consolidation_input(&consumed_messages);
                let assistant_resp = response.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::memory::consolidation::consolidate_turn(
                        provider.as_ref(),
                        &model,
                        mem.as_ref(),
                        &user_msg,
                        &assistant_resp,
                    )
                    .await
                    {
                        tracing::debug!("WS memory consolidation skipped: {e}");
                    }
                });
            }

            let done = serde_json::json!({
                "type": "done",
                "full_response": response,
            });
            let _ = outbound_tx
                .send(Message::Text(done.to_string().into()))
                .await;

            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "idle", None);
            }

            let _ = state.event_tx.send(serde_json::json!({
                "type": "agent_end",
                "provider": provider_label,
                "model": state.model,
            }));
        }
        Err(failure) => {
            persist_turn_transcript(
                state.session_backend.as_ref(),
                session_key,
                &failure.consumed_messages,
                Some(&failure.committed_response),
            );

            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "error", Some(&turn_id));
            }

            tracing::error!(error = %failure.error, "Agent turn failed");
            let sanitized = crate::providers::sanitize_api_error(&failure.error.to_string());
            let error_code = if sanitized.to_lowercase().contains("api key")
                || sanitized.to_lowercase().contains("authentication")
                || sanitized.to_lowercase().contains("unauthorized")
            {
                "AUTH_ERROR"
            } else if sanitized.to_lowercase().contains("provider")
                || sanitized.to_lowercase().contains("model")
            {
                "PROVIDER_ERROR"
            } else {
                "AGENT_ERROR"
            };
            let err = serde_json::json!({
                "type": "error",
                "message": sanitized,
                "code": error_code,
            });
            let _ = outbound_tx
                .send(Message::Text(err.to_string().into()))
                .await;

            let _ = state.event_tx.send(serde_json::json!({
                "type": "error",
                "component": "ws_chat",
                "message": sanitized,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::session_backend::SessionBackend;
    use axum::{
        Json, Router,
        extract::State as AxumState,
        http::{HeaderMap, StatusCode},
        response::{
            IntoResponse,
            sse::{Event, Sse},
        },
        routing::{get, post},
    };
    use futures_util::{SinkExt, StreamExt};
    use serde_json::Value;
    use std::{sync::Arc, time::Duration};
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};

    #[test]
    fn extract_ws_token_from_authorization_header() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer zc_test123".parse().unwrap());
        assert_eq!(extract_ws_token(&headers, None), Some("zc_test123"));
    }

    #[test]
    fn extract_ws_token_from_subprotocol() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "sec-websocket-protocol",
            "zeroclaw.v1, bearer.zc_sub456".parse().unwrap(),
        );
        assert_eq!(extract_ws_token(&headers, None), Some("zc_sub456"));
    }

    #[test]
    fn extract_ws_token_from_query_param() {
        let headers = HeaderMap::new();
        assert_eq!(
            extract_ws_token(&headers, Some("zc_query789")),
            Some("zc_query789")
        );
    }

    #[test]
    fn extract_ws_token_precedence_header_over_subprotocol() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer zc_header".parse().unwrap());
        headers.insert("sec-websocket-protocol", "bearer.zc_sub".parse().unwrap());
        assert_eq!(
            extract_ws_token(&headers, Some("zc_query")),
            Some("zc_header")
        );
    }

    #[test]
    fn extract_ws_token_precedence_subprotocol_over_query() {
        let mut headers = HeaderMap::new();
        headers.insert("sec-websocket-protocol", "bearer.zc_sub".parse().unwrap());
        assert_eq!(extract_ws_token(&headers, Some("zc_query")), Some("zc_sub"));
    }

    #[test]
    fn extract_ws_token_returns_none_when_empty() {
        let headers = HeaderMap::new();
        assert_eq!(extract_ws_token(&headers, None), None);
    }

    #[test]
    fn extract_ws_token_skips_empty_header_value() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer ".parse().unwrap());
        assert_eq!(
            extract_ws_token(&headers, Some("zc_fallback")),
            Some("zc_fallback")
        );
    }

    #[test]
    fn extract_ws_token_skips_empty_query_param() {
        let headers = HeaderMap::new();
        assert_eq!(extract_ws_token(&headers, Some("")), None);
    }

    #[test]
    fn extract_ws_token_subprotocol_with_multiple_entries() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "sec-websocket-protocol",
            "zeroclaw.v1, bearer.zc_tok, other".parse().unwrap(),
        );
        assert_eq!(extract_ws_token(&headers, None), Some("zc_tok"));
    }

    #[derive(Clone)]
    struct MockProviderServerState {
        requests: Arc<parking_lot::Mutex<Vec<Value>>>,
        first_done_delay: Duration,
        first_delta: &'static str,
        later_delta: &'static str,
        fail_on_call: Option<usize>,
    }

    async fn mock_chat_completions(
        AxumState(state): AxumState<MockProviderServerState>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let call_index = {
            let mut requests = state.requests.lock();
            requests.push(body);
            requests.len()
        };

        if state.fail_on_call == Some(call_index) {
            return (StatusCode::BAD_GATEWAY, "mock provider failure").into_response();
        }

        let delta = if call_index == 1 {
            state.first_delta
        } else {
            state.later_delta
        };
        let first_done_delay = state.first_done_delay;

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, axum::Error>>(4);
        tokio::spawn(async move {
            let chunk = serde_json::json!({
                "choices": [{"delta": {"content": delta}}]
            })
            .to_string();
            let _ = tx.send(Ok(Event::default().data(chunk))).await;
            if call_index == 1 {
                tokio::time::sleep(first_done_delay).await;
            }
            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
        });

        Sse::new(ReceiverStream::new(rx)).into_response()
    }

    async fn start_mock_provider_server(
        state: MockProviderServerState,
    ) -> (
        String,
        tokio::task::JoinHandle<()>,
        Arc<parking_lot::Mutex<Vec<Value>>>,
    ) {
        let requests = state.requests.clone();
        let app = Router::new()
            .route("/v1/chat/completions", post(mock_chat_completions))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), handle, requests)
    }

    fn build_ws_test_state(
        provider_url: &str,
        session_backend: Option<Arc<dyn SessionBackend>>,
    ) -> crate::gateway::AppState {
        build_ws_test_state_with_queue(
            provider_url,
            session_backend,
            Arc::new(crate::gateway::session_queue::SessionActorQueue::new(
                8, 30, 600,
            )),
        )
    }

    fn build_ws_test_state_with_queue(
        provider_url: &str,
        session_backend: Option<Arc<dyn SessionBackend>>,
        session_queue: Arc<crate::gateway::session_queue::SessionActorQueue>,
    ) -> crate::gateway::AppState {
        let mut config = crate::config::Config::default();
        let workspace_dir =
            std::env::temp_dir().join(format!("zeroclaw-ws-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace_dir).unwrap();
        config.workspace_dir = workspace_dir.clone();
        config.config_path = workspace_dir.join("config.toml");
        config.api_key = Some("test-key".to_string());
        config.default_provider = Some(format!("custom:{provider_url}"));
        config.default_model = Some("test-model".to_string());
        config.memory.backend = "none".to_string();
        config.memory.auto_save = false;

        let provider: Arc<dyn crate::providers::Provider> = Arc::from(
            crate::providers::create_provider(&format!("custom:{provider_url}"), Some("test-key"))
                .unwrap(),
        );

        crate::gateway::AppState {
            config: Arc::new(parking_lot::Mutex::new(config)),
            provider,
            model: "test-model".into(),
            temperature: 0.0,
            mem: Arc::from(
                crate::memory::create_memory(
                    &crate::config::MemoryConfig {
                        backend: "none".into(),
                        ..crate::config::MemoryConfig::default()
                    },
                    &workspace_dir,
                    None,
                )
                .unwrap(),
            ),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(crate::security::PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(crate::gateway::GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(crate::gateway::auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(crate::gateway::IdempotencyStore::new(
                Duration::from_secs(300),
                1000,
            )),
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
            event_buffer: Arc::new(crate::gateway::sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(crate::gateway::nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            session_backend,
            session_queue,
            device_registry: None,
            pending_pairings: None,
            canvas_store: crate::tools::canvas::CanvasStore::new(),
            #[cfg(feature = "webauthn")]
            webauthn: None,
        }
    }

    async fn start_ws_test_server(
        state: crate::gateway::AppState,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/ws/chat", get(handle_ws_chat))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("ws://{addr}/ws/chat"), handle)
    }

    async fn recv_json(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        loop {
            match ws
                .next()
                .await
                .expect("websocket frame")
                .expect("websocket message")
            {
                ClientMessage::Text(text) => return serde_json::from_str(&text).unwrap(),
                ClientMessage::Close(frame) => panic!("unexpected close: {frame:?}"),
                _ => {}
            }
        }
    }

    #[derive(Default)]
    struct MockSessionBackend {
        messages: parking_lot::Mutex<
            std::collections::HashMap<String, Vec<crate::providers::ChatMessage>>,
        >,
    }

    impl crate::channels::session_backend::SessionBackend for MockSessionBackend {
        fn load(&self, session_key: &str) -> Vec<crate::providers::ChatMessage> {
            self.messages
                .lock()
                .get(session_key)
                .cloned()
                .unwrap_or_default()
        }

        fn append(
            &self,
            session_key: &str,
            message: &crate::providers::ChatMessage,
        ) -> std::io::Result<()> {
            self.messages
                .lock()
                .entry(session_key.to_string())
                .or_default()
                .push(message.clone());
            Ok(())
        }

        fn remove_last(&self, _session_key: &str) -> std::io::Result<bool> {
            Ok(false)
        }

        fn list_sessions(&self) -> Vec<String> {
            self.messages.lock().keys().cloned().collect()
        }
    }

    #[test]
    fn flatten_consolidation_input_matches_existing_plain_text_shape() {
        let rendered = flatten_consolidation_input(&["first".into(), "second".into()]);
        assert_eq!(rendered, "first\n\nsecond");
    }

    #[test]
    fn parse_user_message_content_rejects_unknown_message_type() {
        let err = parse_user_message_content(r#"{"type":"connect"}"#).unwrap_err();
        let Message::Text(payload) = err else {
            panic!("expected text error message");
        };
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["code"], "UNKNOWN_MESSAGE_TYPE");
    }

    #[test]
    fn parse_user_message_content_preserves_original_whitespace() {
        let content =
            parse_user_message_content("{\"type\":\"message\",\"content\":\"  first  \"}")
                .expect("message should be accepted");
        assert_eq!(content, "  first  ");
    }

    #[tokio::test]
    async fn accept_user_text_frame_enqueues_messages_and_reports_overflow() {
        let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(1);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<Message>(4);

        accept_user_text_frame(
            r#"{"type":"message","content":"first"}"#,
            &steering_tx,
            &outbound_tx,
        )
        .await
        .unwrap();

        assert_eq!(steering_rx.recv().await.as_deref(), Some("first"));

        steering_tx.try_send("already queued".into()).unwrap();
        accept_user_text_frame(
            r#"{"type":"message","content":"overflow"}"#,
            &steering_tx,
            &outbound_tx,
        )
        .await
        .unwrap();

        let Some(Message::Text(payload)) = outbound_rx.recv().await else {
            panic!("expected outbound error");
        };
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["code"], "STEERING_QUEUE_FULL");
    }

    #[tokio::test]
    async fn websocket_steering_integration_reuses_first_message_enqueue_path() {
        let provider_state = MockProviderServerState {
            requests: Arc::new(parking_lot::Mutex::new(Vec::new())),
            first_done_delay: Duration::from_millis(250),
            first_delta: "draft",
            later_delta: "final answer",
            fail_on_call: None,
        };
        let (provider_url, provider_handle, requests) =
            start_mock_provider_server(provider_state).await;
        let backend_impl = Arc::new(MockSessionBackend::default());
        let backend: Arc<dyn SessionBackend> = backend_impl.clone();
        let state = build_ws_test_state(&provider_url, Some(backend));
        let (ws_base_url, ws_handle) = start_ws_test_server(state).await;
        let ws_url = format!("{ws_base_url}?session_id=steering-it");

        let (mut ws, _) = connect_async(&ws_url).await.unwrap();
        let session_start = recv_json(&mut ws).await;
        assert_eq!(session_start["type"], "session_start");

        ws.send(ClientMessage::Text(
            r#"{"type":"message","content":"first"}"#.into(),
        ))
        .await
        .unwrap();

        let mut sent_second = false;
        let mut done_count = 0;
        let mut saw_final_chunk = false;

        let final_response = loop {
            let payload = recv_json(&mut ws).await;
            match payload["type"].as_str().unwrap() {
                "chunk" if payload["content"] == "draft" && !sent_second => {
                    ws.send(ClientMessage::Text(
                        r#"{"type":"message","content":"second"}"#.into(),
                    ))
                    .await
                    .unwrap();
                    sent_second = true;
                }
                "chunk" if payload["content"] == "final answer" => {
                    saw_final_chunk = true;
                }
                "chunk_reset" => panic!("websocket steering must not emit chunk_reset"),
                "done" => {
                    done_count += 1;
                    break payload["full_response"].as_str().unwrap().to_string();
                }
                _ => {}
            }
        };

        assert!(sent_second);
        assert_eq!(done_count, 1);
        assert!(saw_final_chunk);
        assert_eq!(final_response, "draftfinal answer");

        let requests = requests.lock().clone();
        assert_eq!(requests.len(), 2);
        let first_user_contents: Vec<_> = requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|msg| msg["role"] == "user")
            .map(|msg| msg["content"].to_string())
            .collect();
        let second_user_contents: Vec<_> = requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|msg| msg["role"] == "user")
            .map(|msg| msg["content"].to_string())
            .collect();
        let second_assistant_contents: Vec<_> = requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|msg| msg["role"] == "assistant")
            .map(|msg| msg["content"].to_string())
            .collect();
        assert_eq!(first_user_contents.len(), 1);
        assert!(first_user_contents[0].contains("first"));
        assert_eq!(second_user_contents.len(), 2);
        assert!(second_user_contents[0].contains("first"));
        assert!(second_user_contents[1].contains("second"));
        assert!(
            second_assistant_contents
                .iter()
                .any(|content| content.contains("draft"))
        );

        let transcript = backend_impl.load("gw_steering-it");
        assert_eq!(transcript.len(), 3);
        assert_eq!(transcript[0].role, "user");
        assert_eq!(transcript[0].content, "first");
        assert_eq!(transcript[1].role, "user");
        assert_eq!(transcript[1].content, "second");
        assert_eq!(transcript[2].role, "assistant");
        assert_eq!(transcript[2].content, "draftfinal answer");

        ws.close(None).await.unwrap();
        provider_handle.abort();
        ws_handle.abort();
    }

    #[tokio::test]
    async fn websocket_steering_overflow_returns_error_and_skips_persistence() {
        let provider_state = MockProviderServerState {
            requests: Arc::new(parking_lot::Mutex::new(Vec::new())),
            first_done_delay: Duration::from_millis(400),
            first_delta: "draft",
            later_delta: "settled",
            fail_on_call: None,
        };
        let (provider_url, provider_handle, _requests) =
            start_mock_provider_server(provider_state).await;
        let backend_impl = Arc::new(MockSessionBackend::default());
        let backend: Arc<dyn SessionBackend> = backend_impl.clone();
        let state = build_ws_test_state(&provider_url, Some(backend));
        let (ws_base_url, ws_handle) = start_ws_test_server(state).await;
        let ws_url = format!("{ws_base_url}?session_id=overflow-it");

        let (mut ws, _) = connect_async(&ws_url).await.unwrap();
        let _ = recv_json(&mut ws).await;

        ws.send(ClientMessage::Text(
            r#"{"type":"message","content":"first"}"#.into(),
        ))
        .await
        .unwrap();

        loop {
            let payload = recv_json(&mut ws).await;
            if payload["type"] == "chunk" && payload["content"] == "draft" {
                break;
            }
        }

        for idx in 0..80 {
            ws.send(ClientMessage::Text(
                format!(r#"{{"type":"message","content":"overflow-{idx}"}}"#).into(),
            ))
            .await
            .unwrap();
        }

        let mut saw_queue_full = false;
        loop {
            let payload = recv_json(&mut ws).await;
            match payload["type"].as_str().unwrap() {
                "error" if payload["code"] == "STEERING_QUEUE_FULL" => saw_queue_full = true,
                "done" => break,
                _ => {}
            }
        }

        assert!(saw_queue_full);
        let transcript = backend_impl.load("gw_overflow-it");
        let user_messages: Vec<_> = transcript.iter().filter(|msg| msg.role == "user").collect();
        assert_eq!(user_messages.len(), 65);
        assert!(user_messages.iter().any(|msg| msg.content == "overflow-63"));
        assert!(user_messages.iter().all(|msg| msg.content != "overflow-79"));

        ws.close(None).await.unwrap();
        provider_handle.abort();
        ws_handle.abort();
    }

    #[tokio::test]
    async fn websocket_session_queue_full_returns_error_without_persisting_turn() {
        let provider_state = MockProviderServerState {
            requests: Arc::new(parking_lot::Mutex::new(Vec::new())),
            first_done_delay: Duration::from_millis(300),
            first_delta: "draft",
            later_delta: "settled",
            fail_on_call: None,
        };
        let (provider_url, provider_handle, _requests) =
            start_mock_provider_server(provider_state).await;
        let backend_impl = Arc::new(MockSessionBackend::default());
        let backend: Arc<dyn SessionBackend> = backend_impl.clone();
        let session_queue = Arc::new(crate::gateway::session_queue::SessionActorQueue::new(
            1, 1, 600,
        ));
        let state = build_ws_test_state_with_queue(&provider_url, Some(backend), session_queue);
        let (ws_base_url, ws_handle) = start_ws_test_server(state).await;
        let ws_url = format!("{ws_base_url}?session_id=queue-full-it");

        let (mut ws1, _) = connect_async(&ws_url).await.unwrap();
        let _ = recv_json(&mut ws1).await;
        ws1.send(ClientMessage::Text(
            r#"{"type":"message","content":"first"}"#.into(),
        ))
        .await
        .unwrap();
        loop {
            let payload = recv_json(&mut ws1).await;
            if payload["type"] == "chunk" && payload["content"] == "draft" {
                break;
            }
        }

        let (mut ws2, _) = connect_async(&ws_url).await.unwrap();
        let _ = recv_json(&mut ws2).await;
        ws2.send(ClientMessage::Text(
            r#"{"type":"message","content":"blocked"}"#.into(),
        ))
        .await
        .unwrap();

        loop {
            let payload = recv_json(&mut ws2).await;
            if payload["type"] == "error" {
                assert_eq!(payload["code"], "SESSION_QUEUE_FULL");
                break;
            }
        }

        loop {
            let payload = recv_json(&mut ws1).await;
            if payload["type"] == "done" {
                break;
            }
        }

        let transcript = backend_impl.load("gw_queue-full-it");
        assert!(
            transcript
                .iter()
                .any(|msg| msg.role == "user" && msg.content == "first")
        );
        assert!(transcript.iter().all(|msg| msg.content != "blocked"));

        ws1.close(None).await.unwrap();
        ws2.close(None).await.unwrap();
        provider_handle.abort();
        ws_handle.abort();
    }

    #[tokio::test]
    async fn websocket_session_queue_timeout_returns_error_without_persisting_turn() {
        let provider_state = MockProviderServerState {
            requests: Arc::new(parking_lot::Mutex::new(Vec::new())),
            first_done_delay: Duration::from_millis(50),
            first_delta: "draft",
            later_delta: "settled",
            fail_on_call: None,
        };
        let (provider_url, provider_handle, _requests) =
            start_mock_provider_server(provider_state).await;
        let backend_impl = Arc::new(MockSessionBackend::default());
        let backend: Arc<dyn SessionBackend> = backend_impl.clone();
        let session_queue = Arc::new(crate::gateway::session_queue::SessionActorQueue::new(
            2, 1, 600,
        ));
        let guard = session_queue.acquire("gw_timeout-it").await.unwrap();
        let state = build_ws_test_state_with_queue(&provider_url, Some(backend), session_queue);
        let (ws_base_url, ws_handle) = start_ws_test_server(state).await;
        let ws_url = format!("{ws_base_url}?session_id=timeout-it");

        let (mut ws, _) = connect_async(&ws_url).await.unwrap();
        let _ = recv_json(&mut ws).await;
        ws.send(ClientMessage::Text(
            r#"{"type":"message","content":"blocked"}"#.into(),
        ))
        .await
        .unwrap();

        loop {
            let payload = recv_json(&mut ws).await;
            if payload["type"] == "error" {
                assert_eq!(payload["code"], "SESSION_QUEUE_TIMEOUT");
                break;
            }
        }

        drop(guard);
        assert!(backend_impl.load("gw_timeout-it").is_empty());

        ws.close(None).await.unwrap();
        provider_handle.abort();
        ws_handle.abort();
    }

    #[tokio::test]
    async fn websocket_error_persists_committed_partial_assistant_output() {
        let provider_state = MockProviderServerState {
            requests: Arc::new(parking_lot::Mutex::new(Vec::new())),
            first_done_delay: Duration::from_millis(250),
            first_delta: "draft",
            later_delta: "unused",
            fail_on_call: Some(2),
        };
        let (provider_url, provider_handle, _requests) =
            start_mock_provider_server(provider_state).await;
        let backend_impl = Arc::new(MockSessionBackend::default());
        let backend: Arc<dyn SessionBackend> = backend_impl.clone();
        let state = build_ws_test_state(&provider_url, Some(backend));
        let (ws_base_url, ws_handle) = start_ws_test_server(state).await;
        let ws_url = format!("{ws_base_url}?session_id=partial-it");

        let (mut ws, _) = connect_async(&ws_url).await.unwrap();
        let _ = recv_json(&mut ws).await;

        ws.send(ClientMessage::Text(
            r#"{"type":"message","content":"first"}"#.into(),
        ))
        .await
        .unwrap();

        let mut sent_second = false;
        loop {
            let payload = recv_json(&mut ws).await;
            match payload["type"].as_str().unwrap() {
                "chunk" if payload["content"] == "draft" && !sent_second => {
                    ws.send(ClientMessage::Text(
                        r#"{"type":"message","content":"second"}"#.into(),
                    ))
                    .await
                    .unwrap();
                    sent_second = true;
                }
                "error" => break,
                _ => {}
            }
        }

        assert!(sent_second);
        let transcript = backend_impl.load("gw_partial-it");
        assert_eq!(transcript.len(), 3);
        assert_eq!(transcript[0].role, "user");
        assert_eq!(transcript[0].content, "first");
        assert_eq!(transcript[1].role, "user");
        assert_eq!(transcript[1].content, "second");
        assert_eq!(transcript[2].role, "assistant");
        assert_eq!(transcript[2].content, "draft");

        ws.close(None).await.unwrap();
        provider_handle.abort();
        ws_handle.abort();
    }
}
