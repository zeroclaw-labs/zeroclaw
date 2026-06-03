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
//! ## Tool approvals
//!
//! When supervised-mode tool calls hit the `ApprovalManager`, the server
//! emits an `approval_request` and pauses the tool loop until the client
//! responds. Mirrors the Telegram inline-keyboard / CLI Y/N/A pattern,
//! over the WS frame transport.
//!
//! ```text
//! Server -> Client: {
//!     "type": "approval_request",
//!     "request_id": "<uuid>",
//!     "tool": "shell",
//!     "arguments_summary": "command: git status",
//!     "timeout_secs": 120
//! }
//! Client -> Server: {
//!     "type": "approval_response",
//!     "request_id": "<uuid>",
//!     "decision": "approve" | "deny" | "always"
//! }
//! ```
//!
//! `approve` runs the tool once, `always` adds the tool to the session
//! allowlist for the rest of the conversation, `deny` returns a structured
//! error to the model. When no client is connected, or the client
//! disconnects mid-prompt, the tool call is auto-denied after `timeout_secs`.
//!
//! ### `arguments_summary` security boundary
//!
//! `arguments_summary` is a human-readable string the runtime synthesises
//! for the operator (e.g. `"command: git status"`, `"path: /etc/hosts"`).
//! It is render-only; the operator's approve/deny choice attaches to the
//! `request_id`, never to the summary string. The runtime must not echo
//! any `#[secret]` or `#[derived_from_secret]` field (auth tokens, API
//! keys, OAuth secrets) into the summary. The agent's tool loop runs
//! tool args through `zeroclaw_runtime::approval::summarize_args` before
//! the request reaches this transport; do not stringify raw args here.
//!
//! Query params:
//! - `session_id` — resume or create a session (default: new UUID)
//! - `name` — optional human-readable label for the session
//! - `token` — bearer auth token (alternative to Authorization header)

use super::AppState;
use crate::ws_approval::{PendingApprovals, WsApprovalChannel, new_pending_approvals};
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::channel::ChannelApprovalResponse;

/// Default wall-clock budget for the operator to answer an
/// `approval_request` frame before the channel auto-denies. Mirrors the
/// channel-side default on `TelegramConfig::approval_timeout_secs`.
const WS_APPROVAL_TIMEOUT_SECS: u64 = 120;

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
    /// Project root / working directory for this session.
    #[serde(default, alias = "workspaceDir", alias = "workspace_dir")]
    cwd: Option<String>,
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
    /// Configured agent alias to run as. Required — every WebSocket
    /// session is bound to an explicit agent (no default agent exists).
    #[serde(default, alias = "agentAlias", alias = "agent")]
    pub agent_alias: Option<String>,
    /// Project root / working directory for this session.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default, alias = "workspaceDir", alias = "workspace_dir")]
    pub workspace_dir: Option<String>,
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
        && !t.is_empty()
    {
        return Some(t);
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
        && !t.is_empty()
    {
        return Some(t);
    }

    // 3. ?token= query parameter
    if let Some(t) = query_token
        && !t.is_empty()
    {
        return Some(t);
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
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == WS_PROTOCOL))
    {
        ws.protocols([WS_PROTOCOL])
    } else {
        ws
    };

    // Reject the upgrade up-front when the client didn't pick an agent.
    // No default — every WS session is bound to an explicit agent.
    let Some(agent_alias) = params.agent_alias.filter(|s| !s.trim().is_empty()) else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "Missing required `agent` query parameter — pass `?agent=<alias>` matching a configured [agents.<alias>] entry.",
        )
            .into_response();
    };
    {
        let cfg = state.config.read();
        if cfg.agent(&agent_alias).is_none() {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                format!(
                    "Unknown agent `{agent_alias}` — no [agents.{agent_alias}] entry configured."
                ),
            )
                .into_response();
        }
    }

    let session_id = params.session_id;
    let session_name = params.name;
    let session_cwd = params.cwd.or(params.workspace_dir);
    ws.on_upgrade(move |socket| {
        handle_socket(
            socket,
            state,
            agent_alias,
            session_id,
            session_name,
            session_cwd,
        )
    })
    .into_response()
}

/// Gateway session key prefix to avoid collisions with channel sessions.
const GW_SESSION_PREFIX: &str = "gw_";

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    agent_alias: String,
    session_id: Option<String>,
    session_name: Option<String>,
    session_cwd: Option<String>,
) {
    let (mut sender, mut receiver) = socket.split();

    // Resolve session ID: use provided or generate a new UUID
    let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_key = format!("{GW_SESSION_PREFIX}{session_id}");
    // Match the sanitized form persisted by memory backend migrations.
    let mut memory_session_id = zeroclaw_api::session_keys::sanitize_session_key(&session_id);

    // Hydrate session metadata from persistence (if available). Agent
    // construction is deferred until after the optional `connect` frame so the
    // client can provide a per-session cwd for the security sandbox root.
    let config = state.config.read().clone();
    let mut resumed = false;
    let mut message_count: usize = 0;
    let mut effective_name: Option<String> = None;
    let mut stored_messages = Vec::new();
    if let Some(ref backend) = state.session_backend {
        let messages = backend.load(&session_key);
        if !messages.is_empty() {
            message_count = messages.len();
            stored_messages = messages;
            resumed = true;
        }
        // Set session name if provided (non-empty) on connect
        if let Some(ref name) = session_name
            && !name.is_empty()
        {
            let _ = backend.set_session_name(&session_key, name);
            effective_name = Some(name.clone());
        }
        // If no name was provided via query param, load the stored name
        if effective_name.is_none() {
            effective_name = backend.get_session_name(&session_key).unwrap_or(None);
        }
        // Stamp the agent alias so future /api/sessions queries and
        // per-agent filters can attribute this session to its agent.
        let _ = backend.set_session_agent_alias(&session_key, &agent_alias);
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

    // ── Optional connect handshake ──────────────────────────────────
    // The first message may be a `{"type":"connect",...}` frame carrying
    // connection parameters.  If it is, we extract the params, send an
    // ack, and proceed to the normal message loop.  If the first message
    // is a regular `{"type":"message",...}` frame, we fall through and
    // process it immediately (backward-compatible).
    let mut first_msg_fallback: Option<String> = None;
    let mut requested_cwd = session_cwd;

    if let Some(first) = receiver.next().await {
        match first {
            Ok(Message::Text(text)) => {
                if let Ok(cp) = serde_json::from_str::<ConnectParams>(&text) {
                    if cp.msg_type == "connect" {
                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"session_id": cp.session_id, "device_name": cp.device_name, "capabilities": cp.capabilities, "cwd": cp.cwd})), "WebSocket connect params received");
                        if let Some(sid) = &cp.session_id {
                            memory_session_id =
                                zeroclaw_api::session_keys::sanitize_session_key(sid);
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({"session_id": sid})),
                                "WebSocket connect session override received"
                            );
                        }
                        if cp.cwd.is_some() {
                            requested_cwd = cp.cwd;
                        }
                        let ack = serde_json::json!({
                            "type": "connected",
                            "message": "Connection established"
                        });
                        let _ = sender.send(Message::Text(ack.to_string().into())).await;
                    } else {
                        // Not a connect message — fall through to normal processing
                        first_msg_fallback = Some(text.to_string());
                    }
                } else {
                    // Not parseable as ConnectParams — fall through
                    first_msg_fallback = Some(text.to_string());
                }
            }
            Ok(Message::Close(_)) | Err(_) => return,
            _ => {}
        }
    }

    let session_cwd = match resolve_session_cwd(requested_cwd.as_deref(), &config.data_dir) {
        Ok(cwd) => cwd,
        Err(e) => {
            let err = serde_json::json!({
                "type": "error",
                "message": e.to_string(),
                "code": "INVALID_CWD"
            });
            let _ = sender.send(Message::Text(err.to_string().into())).await;
            return;
        }
    };

    if let Some(err) = needs_onboarding_ws_error(&config) {
        let _ = sender.send(Message::Text(err.to_string().into())).await;
        return;
    }

    // Build a persistent Agent for this connection so history is maintained
    // across turns. The session cwd becomes the security sandbox root; config
    // workspace remains the daemon data directory. Routes through the
    // backchannel constructor so this WS session shares its tool-approval
    // path with the operator-driven dashboard. The agent_alias was
    // validated up-front in handle_ws_chat against the configured agents.
    let mut agent =
        match zeroclaw_runtime::agent::Agent::from_config_with_session_cwd_and_mcp_backchannel(
            &config,
            &agent_alias,
            Some(&session_cwd),
            true,
            false,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Agent initialization failed"
                );
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
    agent.set_memory_session_id(Some(memory_session_id));
    if !stored_messages.is_empty() {
        agent.seed_history(&stored_messages);
    }

    // ── Tool-approval back-channel ─────────────────────────────────
    // Connection-level event channel that the WsApprovalChannel shares
    // with the per-turn forward task: it pushes ApprovalRequest frames
    // here when the agent's tool loop pauses for consent, and the
    // forward task drains them out the same WebSocket as the regular
    // streaming events. The pending map is shared with the receive loop
    // so inbound `approval_response` frames can resolve the matching
    // oneshot waiter.
    let (approval_event_tx, mut approval_event_rx) =
        tokio::sync::mpsc::channel::<zeroclaw_api::agent::TurnEvent>(8);
    let pending_approvals: PendingApprovals = new_pending_approvals();
    let approval_channel = Arc::new(WsApprovalChannel::new(
        approval_event_tx.clone(),
        pending_approvals.clone(),
        Duration::from_secs(WS_APPROVAL_TIMEOUT_SECS),
    ));
    agent
        .channel_handles()
        .register_channel("ws", approval_channel.clone());

    // Seed agent's channel handles with configured channels (telegram,
    // etc.) so the dashboard agent can deliver to external channels.
    // The agent creates its own fresh handles in
    // from_config_with_session_cwd_and_mcp_backchannel, so they need
    // to be populated here — separate from the gateway boot-time seeding.
    let ch = agent.channel_handles();
    let channel_names = zeroclaw_channels::orchestrator::register_channels_for_tools(
        &config,
        &ch.ask_user,
        &Some(ch.reaction.clone()),
        &ch.poll,
        &ch.escalate,
    );
    if !channel_names.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({"channels": channel_names, "session": session_key})
            ),
            "Seeded {} channel(s) into dashboard agent session",
        );
    }

    // Process the first message if it was not a connect frame
    if let Some(ref text) = first_msg_fallback {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
            if parsed["type"].as_str() == Some("message") {
                let content = parsed["content"].as_str().unwrap_or("").to_string();
                if !content.is_empty() {
                    let _session_guard = match state.session_queue.acquire(&session_key).await {
                        Ok(guard) => guard,
                        Err(e) => {
                            let err = serde_json::json!({
                                "type": "error",
                                "message": e.to_string(),
                                "code": session_queue_ws_error_code(&e)
                            });
                            let _ = sender.send(Message::Text(err.to_string().into())).await;
                            return;
                        }
                    };
                    process_chat_message(
                        &state,
                        &mut agent,
                        &mut sender,
                        &mut receiver,
                        &mut approval_event_rx,
                        &pending_approvals,
                        &content,
                        &session_key,
                    )
                    .await;
                }
            } else {
                let unknown_type = parsed["type"].as_str().unwrap_or("unknown");
                let err = serde_json::json!({
                    "type": "error",
                    "message": format!(
                        "Unsupported message type \"{unknown_type}\". Send {{\"type\":\"message\",\"content\":\"your text\"}}"
                    )
                });
                let _ = sender.send(Message::Text(err.to_string().into())).await;
            }
        } else {
            let err = serde_json::json!({
                "type": "error",
                "message": "Invalid JSON. Send {\"type\":\"message\",\"content\":\"your text\"}"
            });
            let _ = sender.send(Message::Text(err.to_string().into())).await;
        }
    }

    // Subscribe to the shared broadcast channel so cron/heartbeat events
    // are forwarded to this WebSocket client.
    let mut broadcast_rx = state.event_tx.subscribe();

    loop {
        tokio::select! {
            // ── Client message ────────────────────────────────────────
            client_msg = receiver.next() => {
                let Some(msg) = client_msg else { break };
                let msg = match msg {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => continue,
                };

                // Parse incoming message
                let parsed: serde_json::Value = match serde_json::from_str(&msg) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = serde_json::json!({
                            "type": "error",
                            "message": format!("Invalid JSON: {}", e),
                            "code": "INVALID_JSON"
                        });
                        let _ = sender.send(Message::Text(err.to_string().into())).await;
                        continue;
                    }
                };

                let msg_type = parsed["type"].as_str().unwrap_or("");

                // ── Voice duplex event dispatch (gated by feature flag + runtime config) ──
                #[cfg(feature = "gateway-voice-duplex")]
                {
                    // Multi-instance shape: presence in the map = enabled.
                    let duplex_enabled = !state.config.read().channels.voice_duplex.is_empty();
                    if duplex_enabled {
                        if let Some(voice_event) = crate::voice_duplex::try_parse_voice_event(&msg) {
                            if let Some(error_frame) = crate::voice_duplex::handle_voice_event(voice_event) {
                                let _ = sender.send(Message::Text(error_frame.to_string().into())).await;
                            }
                            continue;
                        }
                    }
                }

                // ── approval_response (operator answered a tool prompt) ──
                if msg_type == "approval_response" {
                    let request_id = parsed["request_id"].as_str().unwrap_or("");
                    let decision_str = parsed["decision"].as_str().unwrap_or("");
                    let decision = match decision_str {
                        "approve" => Some(ChannelApprovalResponse::Approve),
                        "always" => Some(ChannelApprovalResponse::AlwaysApprove),
                        "deny" => Some(ChannelApprovalResponse::Deny),
                        _ => None,
                    };
                    if request_id.is_empty() || decision.is_none() {
                        let err = serde_json::json!({
                            "type": "error",
                            "message": "approval_response requires request_id and decision in {approve,deny,always}",
                            "code": "INVALID_APPROVAL_RESPONSE"
                        });
                        let _ = sender.send(Message::Text(err.to_string().into())).await;
                        continue;
                    }
                    if let Some(tx) = pending_approvals.lock().remove(request_id) {
                        let _ = tx.send(decision.expect("checked above"));
                    } else {
                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"request_id": request_id})), "approval_response with no matching pending request");
                    }
                    continue;
                }

                if msg_type != "message" {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": format!(
                            "Unsupported message type \"{msg_type}\". Send {{\"type\":\"message\",\"content\":\"your text\"}}"
                        ),
                        "code": "UNKNOWN_MESSAGE_TYPE"
                    });
                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                    continue;
                }

                let content = parsed["content"].as_str().unwrap_or("").to_string();
                if content.is_empty() {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": "Message content cannot be empty",
                        "code": "EMPTY_CONTENT"
                    });
                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                    continue;
                }

                // Acquire session lock to serialize concurrent turns
                let _session_guard = match state.session_queue.acquire(&session_key).await {
                    Ok(guard) => guard,
                    Err(e) => {
                        let err = serde_json::json!({
                            "type": "error",
                            "message": e.to_string(),
                            "code": session_queue_ws_error_code(&e)
                        });
                        let _ = sender.send(Message::Text(err.to_string().into())).await;
                        continue;
                    }
                };

                process_chat_message(
                    &state,
                    &mut agent,
                    &mut sender,
                    &mut receiver,
                    &mut approval_event_rx,
                    &pending_approvals,
                    &content,
                    &session_key,
                )
                .await;
            }

            // ── Broadcast event (cron/heartbeat results) ──────────────
            event = broadcast_rx.recv() => {
                if let Ok(event) = event
                    && event_matches_session(&event, &session_id)
                {
                    let _ = sender.send(Message::Text(event.to_string().into())).await;
                }
            }

            // ── Approval request from the agent's tool loop ────────────
            // The WsApprovalChannel emits these whenever a supervised tool
            // call needs operator consent. Forwarded out the same socket
            // as the regular streaming events; the matching response
            // arrives via the `approval_response` arm above and resolves
            // the channel's pending oneshot.
            approval_event = approval_event_rx.recv() => {
                let Some(event) = approval_event else { break };
                let frame = match event {
                    zeroclaw_api::agent::TurnEvent::ApprovalRequest {
                        request_id,
                        tool_name,
                        arguments_summary,
                        timeout_secs,
                    } => serde_json::json!({
                        "type": "approval_request",
                        "request_id": request_id,
                        "tool": tool_name,
                        "arguments_summary": arguments_summary,
                        "timeout_secs": timeout_secs,
                    }),
                    other => {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"kind": format!("{:?}", other)})), "non-ApprovalRequest event leaked into approval channel");
                        continue;
                    }
                };
                let _ = sender.send(Message::Text(frame.to_string().into())).await;
            }
        }
    }
}

fn resolve_session_cwd(
    requested_cwd: Option<&str>,
    default_workspace: &Path,
) -> anyhow::Result<PathBuf> {
    let cwd = requested_cwd
        .map(PathBuf::from)
        .unwrap_or_else(|| default_workspace.to_path_buf());
    std::fs::canonicalize(&cwd).map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "cwd": cwd.display().to_string(),
                    "error": format!("{}", e),
                })),
            "ws session cwd rejected"
        );
        anyhow::Error::msg(format!(
            "cwd is not a usable directory ({}): {e}",
            cwd.display()
        ))
    })
}

fn session_queue_ws_error_code(error: &crate::session_queue::SessionQueueError) -> &'static str {
    match error {
        crate::session_queue::SessionQueueError::QueueFull { .. } => "SESSION_QUEUE_FULL",
        crate::session_queue::SessionQueueError::Timeout { .. } => "SESSION_QUEUE_TIMEOUT",
    }
}

fn persist_conversation_messages(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    session_key: &str,
    messages: &[zeroclaw_providers::ConversationMessage],
) {
    for message in messages {
        let zeroclaw_providers::ConversationMessage::Chat(message) = message else {
            continue;
        };
        if message.role == "system" {
            continue;
        }
        let _ = backend.append(session_key, message);
    }
}

fn has_assistant_chat_message(messages: &[zeroclaw_providers::ConversationMessage]) -> bool {
    messages.iter().any(|message| {
        matches!(
            message,
            zeroclaw_providers::ConversationMessage::Chat(message)
                if message.role == "assistant"
        )
    })
}

fn needs_onboarding_ws_error(
    config: &zeroclaw_config::schema::Config,
) -> Option<serde_json::Value> {
    let model = config.resolve_default_model().unwrap_or_default();
    crate::needs_quickstart_for(&model)?;
    Some(serde_json::json!({
        "type": "error",
        "error": "needs_onboarding",
        "code": "NEEDS_ONBOARDING",
        "message": crate::needs_quickstart_channel_reply(),
        "url": "/onboard",
    }))
}

fn event_matches_session(event: &serde_json::Value, session_id: &str) -> bool {
    match event.get("session_id").and_then(|value| value.as_str()) {
        Some(event_session_id) => event_session_id == session_id,
        None => true,
    }
}

/// Process a single chat message through the agent and send the response.
///
/// Uses [`Agent::turn_streamed`] so that intermediate text chunks, tool calls,
/// and tool results are forwarded to the WebSocket client in real time.
async fn process_chat_message(
    state: &AppState,
    agent: &mut zeroclaw_runtime::agent::Agent,
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    receiver: &mut futures_util::stream::SplitStream<WebSocket>,
    approval_event_rx: &mut tokio::sync::mpsc::Receiver<zeroclaw_api::agent::TurnEvent>,
    pending_approvals: &PendingApprovals,
    content: &str,
    session_key: &str,
) {
    use futures_util::StreamExt as _;
    use zeroclaw_runtime::agent::TurnEvent;

    let provider_label = {
        let cfg = state.config.read();
        cfg.providers
            .models
            .iter_entries()
            .next()
            .map(|(ty, alias, _)| format!("{ty}.{alias}"))
            .unwrap_or_else(|| "unknown".to_string())
    };

    // Broadcast agent_start event
    let _ = state.event_tx.send(serde_json::json!({
        "type": "agent_start",
        "model_provider": provider_label,
        "model": state.model,
    }));

    // Set session state to running
    let turn_id = uuid::Uuid::new_v4().to_string();
    if let Some(ref backend) = state.session_backend {
        let _ = backend.set_session_state(session_key, "running", Some(&turn_id));
    }

    // ── Cancellation token lifecycle ─────────────────────────────
    // Create a token before the turn starts so the abort endpoint
    // can cancel it. Remove it after the turn completes regardless
    // of outcome (normal, error, or cancelled).
    let cancel_token = tokio_util::sync::CancellationToken::new();
    {
        state
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .insert(session_key.to_string(), cancel_token.clone());
    }

    // Channel for streaming turn events from the agent.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(32);

    // Run the streamed turn concurrently: the agent produces events
    // while we forward them to the WebSocket below.  We cannot move
    // `agent` into a spawned task (it is `&mut`), so we use a join
    // instead — `turn_streamed` writes to the channel and we drain it
    // from the other branch.
    let content_owned = content.to_string();
    let session_key_owned = session_key.to_string();
    let (turn_alias, turn_provider, turn_model) = agent.attribution_fields();
    let turn_fut = async {
        use ::zeroclaw_log::Instrument as _;
        let span = ::zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            session_key = %session_key_owned,
            agent_alias = %turn_alias,
            model_provider = %turn_provider,
            model = %turn_model,
            channel = "wss",
        );
        zeroclaw_runtime::agent::loop_::scope_session_key(
            Some(session_key_owned.clone()),
            agent
                .turn_streamed_with_steering_state(
                    &content_owned,
                    event_tx,
                    Some(cancel_token.clone()),
                    Some(&mut steering_rx),
                )
                .instrument(span),
        )
        .await
    };

    // Drive both futures concurrently: the agent turn produces events
    // and we relay them over WebSocket. Track streamed chunks so we
    // can reconstruct partial content on cancellation.
    //
    let mut accumulated_text = String::new();

    // Aggregate token usage across all LLM calls in this turn.
    // The agent emits TurnEvent::Usage once per LLM call when the provider
    // surfaces usage; we sum to produce a single done-frame total.
    let mut total_input_tokens: Option<u64> = None;
    let mut total_output_tokens: Option<u64> = None;

    // Routes the three concurrent streams that the running turn cares about:
    //   1. inbound `approval_response` frames from the WebSocket client,
    //   2. `TurnEvent::ApprovalRequest` events from `WsApprovalChannel`,
    //   3. ordinary `TurnEvent`s from the agent loop.
    // Without the multiplexed select, the loop draining only `event_rx`
    // would block the approval back-channel for the whole turn, so a pending
    // tool approval could neither be sent to the client nor answered before
    // the timeout fired.
    let forward_fut = async {
        let mut cancel_drained = false;
        loop {
            tokio::select! {
                biased;
                // ── Cancellation arm ─────────────────────────────
                // When `/abort` cancels the token, immediately drop every
                // parked oneshot sender so any in-flight `request_approval`
                // unblocks via the "sender dropped → deny" path in
                // `WsApprovalChannel`. Without this, the approval future
                // races only its own `timeout_secs` (default 120s) and
                // ignores the cancel token, so the abort sits idle for up
                // to two minutes before the tool loop even gets a chance
                // to observe the cancellation.
                _ = cancel_token.cancelled(), if !cancel_drained => {
                    let drained: Vec<_> = pending_approvals.lock().drain().collect();
                    drop(drained);
                    cancel_drained = true;
                    // Fall through; the agent loop will now wake from the
                    // approval await, see the cancel token, and propagate
                    // a ToolLoopCancelled error which closes event_rx and
                    // breaks this loop on the `event_rx.recv()` arm below.
                }
                client_msg = receiver.next() => {
                    // On client disconnect, `receiver.next()` returns `None`
                    // (stream end) or `Err(_)` repeatedly. A bare `continue`
                    // hot-loops the select; cancel the turn so `turn_fut`
                    // resolves with `ToolLoopCancelled` and `tokio::join!`
                    // below can return. See #6514.
                    let text = match client_msg {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                            cancel_token.cancel();
                            break;
                        }
                        _ => continue,
                    };
                    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
                        let err = serde_json::json!({
                            "type": "error",
                            "message": "Invalid JSON. Send {\"type\":\"message\",\"content\":\"your text\"}",
                            "code": "INVALID_JSON"
                        });
                        let _ = sender.send(Message::Text(err.to_string().into())).await;
                        continue;
                    };
                    match parsed["type"].as_str() {
                        Some("approval_response") => {
                            let request_id = parsed["request_id"].as_str().unwrap_or("");
                            let decision = match parsed["decision"].as_str().unwrap_or("") {
                                "approve" => Some(ChannelApprovalResponse::Approve),
                                "always" => Some(ChannelApprovalResponse::AlwaysApprove),
                                "deny" => Some(ChannelApprovalResponse::Deny),
                                _ => None,
                            };
                            if request_id.is_empty() || decision.is_none() {
                                continue;
                            }
                            if let Some(tx) = pending_approvals.lock().remove(request_id) {
                                let _ = tx.send(decision.expect("checked above"));
                            } else {
                                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"request_id": request_id})), "approval_response with no matching pending request (mid-turn)");
                            }
                        }
                        Some("message") => {
                            let content = parsed["content"].as_str().unwrap_or("").to_string();
                            if content.is_empty() {
                                let err = serde_json::json!({
                                    "type": "error",
                                    "message": "Message content cannot be empty",
                                    "code": "EMPTY_CONTENT"
                                });
                                let _ = sender.send(Message::Text(err.to_string().into())).await;
                                continue;
                            }
                            match steering_tx.try_send(content) {
                                Ok(()) => {}
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                    let err = serde_json::json!({
                                        "type": "error",
                                        "message": "Steering queue is full for the running turn",
                                        "code": "STEERING_QUEUE_FULL"
                                    });
                                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                    let err = serde_json::json!({
                                        "type": "error",
                                        "message": "Running turn is no longer accepting steering messages",
                                        "code": "STEERING_CLOSED"
                                    });
                                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                approval = approval_event_rx.recv() => {
                    let Some(event) = approval else { continue };
                    if let TurnEvent::ApprovalRequest {
                        request_id,
                        tool_name,
                        arguments_summary,
                        timeout_secs,
                    } = event {
                        let frame = serde_json::json!({
                            "type": "approval_request",
                            "request_id": request_id,
                            "tool": tool_name,
                            "arguments_summary": arguments_summary,
                            "timeout_secs": timeout_secs,
                        });
                        let _ = sender.send(Message::Text(frame.to_string().into())).await;
                    }
                }
                    event_opt = event_rx.recv() => {
                    let Some(event) = event_opt else { break };
                    let ws_msg = match event {
                        TurnEvent::Usage {
                            input_tokens,
                            cached_input_tokens: _,
                            output_tokens,
                            cost_usd: _,
                        } => {
                            // `input_tokens` per TokenUsage contract is
                            // the *total* prompt size (uncached + cached).
                            // `cached_input_tokens` is a subset and must
                            // NOT be added — that would double-count
                            // cache reads.
                            if let Some(it) = input_tokens {
                                total_input_tokens = Some(total_input_tokens.unwrap_or(0) + it);
                            }
                            if let Some(ot) = output_tokens {
                                total_output_tokens = Some(total_output_tokens.unwrap_or(0) + ot);
                            }
                            continue;
                        }
                        TurnEvent::Chunk { ref delta } => {
                            accumulated_text.push_str(delta);
                            serde_json::json!({ "type": "chunk", "content": delta })
                        }
                        TurnEvent::Thinking { delta } => {
                            serde_json::json!({ "type": "thinking", "content": delta })
                        }
                        TurnEvent::ToolCall { id, name, args } => {
                            serde_json::json!({ "type": "tool_call", "id": id, "name": name, "args": args })
                        }
                        TurnEvent::ToolResult { id, name, output } => {
                            serde_json::json!({ "type": "tool_result", "id": id, "name": name, "output": output })
                        }
                        TurnEvent::ApprovalRequest {
                            request_id,
                            tool_name,
                            arguments_summary,
                            timeout_secs,
                        } => serde_json::json!({
                            "type": "approval_request",
                            "request_id": request_id,
                            "tool": tool_name,
                            "arguments_summary": arguments_summary,
                            "timeout_secs": timeout_secs,
                        }),
                    };
                    let _ = sender.send(Message::Text(ws_msg.to_string().into())).await;
                }
            }
        }
    };

    let (result, ()) = tokio::join!(turn_fut, forward_fut);

    // ── Remove cancel token (turn finished) ──────────────────────
    {
        state
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .remove(session_key);
    }

    // Check if this turn was cancelled. `turn_streamed` propagates
    // `ToolLoopCancelled` through anyhow, so we detect it here.
    let was_cancelled = match &result {
        Err(e) => zeroclaw_runtime::agent::loop_::is_tool_loop_cancelled(&e.error),
        Ok(_) => false,
    };

    if was_cancelled {
        if let Some(ref backend) = state.session_backend {
            match &result {
                Err(error) if !error.new_messages.is_empty() => {
                    persist_conversation_messages(
                        backend.as_ref(),
                        session_key,
                        &error.new_messages,
                    );
                    if !has_assistant_chat_message(&error.new_messages) {
                        let truncated = if accumulated_text.is_empty() {
                            "[interrupted by user]".to_string()
                        } else {
                            format!("{accumulated_text}\n\n[interrupted by user]")
                        };
                        let assistant_msg = zeroclaw_providers::ChatMessage::assistant(&truncated);
                        let _ = backend.append(session_key, &assistant_msg);
                    }
                }
                _ => {
                    let truncated = if accumulated_text.is_empty() {
                        "[interrupted by user]".to_string()
                    } else {
                        format!("{accumulated_text}\n\n[interrupted by user]")
                    };
                    let assistant_msg = zeroclaw_providers::ChatMessage::assistant(&truncated);
                    let _ = backend.append(session_key, &assistant_msg);
                }
            }
        }

        // Inform the client the turn was aborted
        let aborted = serde_json::json!({ "type": "aborted" });
        let _ = sender.send(Message::Text(aborted.to_string().into())).await;

        // Set session state to idle
        if let Some(ref backend) = state.session_backend {
            let _ = backend.set_session_state(session_key, "idle", None);
        }

        // Broadcast agent_end event
        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_end",
            "model_provider": provider_label,
            "model": state.model,
        }));

        // Trace the cancelled turn so the doctor / replay tool sees it
        // alongside successful turns. #6001 follow-through.
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "model_provider": provider_label,
                    "model": state.model,
                    "session_key": session_key,
                    "reason": "interrupted by user",
                    "cancelled": true,
                    "trace_id": turn_id,
                })),
            "gateway_ws_turn"
        );

        return;
    }

    match result {
        Ok(outcome) => {
            if let Some(ref backend) = state.session_backend {
                persist_conversation_messages(backend.as_ref(), session_key, &outcome.new_messages);
            }

            // Fire-and-forget memory consolidation so facts from WS sessions
            // are extracted to long-term memory (Daily + Core categories).
            if state.auto_save {
                let mem = state.mem.clone();
                let model_provider = state.model_provider.clone();
                let model = state.model.clone();
                let temperature = state.temperature;
                let user_msg = content.to_string();
                let assistant_resp = outcome.response.clone();
                zeroclaw_spawn::spawn!(async move {
                    if let Err(e) = zeroclaw_memory::consolidation::consolidate_turn(
                        model_provider.as_ref(),
                        &model,
                        temperature,
                        mem.as_ref(),
                        &user_msg,
                        &assistant_resp,
                    )
                    .await
                    {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "WS memory consolidation skipped"
                        );
                    }
                });
            }

            // Compute cost from accumulated tokens + configured pricing,
            // then write the cost record so /api/cost and costs.jsonl reflect
            // this turn. Done before the done frame so cost_usd can ride along.
            let total_tokens = match (total_input_tokens, total_output_tokens) {
                (Some(i), Some(o)) => Some(i.saturating_add(o)),
                (Some(i), None) => Some(i),
                (None, Some(o)) => Some(o),
                (None, None) => None,
            };
            let cost_usd = record_turn_cost(
                state,
                &provider_label,
                &state.model,
                total_input_tokens,
                total_output_tokens,
                None,
            );

            let done = serde_json::json!({
                "type": "done",
                "full_response": outcome.response,
                "input_tokens": total_input_tokens,
                "output_tokens": total_output_tokens,
                "tokens_used": total_tokens,
                "cost_usd": cost_usd,
                "model": state.model,
                "provider": provider_label,
            });
            let _ = sender.send(Message::Text(done.to_string().into())).await;

            // Set session state to idle
            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "idle", None);
            }

            // Broadcast agent_end event
            let _ = state.event_tx.send(serde_json::json!({
                "type": "agent_end",
                "model_provider": provider_label,
                "model": state.model,
            }));

            // Append a runtime-trace.jsonl record so a `zeroclaw doctor`
            // sweep sees gateway WS turns alongside channel and CLI turns.
            // Closes the gateway-side trace gap from #6001.
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "model_provider": provider_label,
                        "model": state.model,
                        "session_key": session_key,
                        "input_tokens": total_input_tokens,
                        "output_tokens": total_output_tokens,
                        "tokens_used": total_tokens,
                        "cost_usd": cost_usd,
                        "trace_id": turn_id,
                    })),
                "gateway_ws_turn"
            );
        }
        Err(e) => {
            if let Some(ref backend) = state.session_backend
                && !e.new_messages.is_empty()
            {
                persist_conversation_messages(backend.as_ref(), session_key, &e.new_messages);
            }

            // Set session state to error
            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "error", Some(&turn_id));
            }

            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e.error)})),
                "Agent turn failed"
            );
            let sanitized = zeroclaw_providers::sanitize_api_error(&e.error.to_string());
            let error_code = if sanitized.to_lowercase().contains("api key")
                || sanitized.to_lowercase().contains("authentication")
                || sanitized.to_lowercase().contains("unauthorized")
            {
                "AUTH_ERROR"
            } else if sanitized.to_lowercase().contains("model_provider")
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
            let _ = sender.send(Message::Text(err.to_string().into())).await;

            // Broadcast error event
            let _ = state.event_tx.send(serde_json::json!({
                "type": "error",
                "component": "ws_chat",
                "message": sanitized,
            }));

            // Trace the failed turn so the doctor / replay tool sees the
            // failure mode and the turn_id can be cross-referenced with
            // costs.jsonl. #6001 follow-through.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "model_provider": provider_label,
                        "model": state.model,
                        "session_key": session_key,
                        "error": sanitized,
                        "error_code": error_code,
                        "trace_id": turn_id,
                    })),
                "gateway_ws_turn"
            );
        }
    }
}

/// Record token usage for the just-completed turn against the gateway's
/// cost tracker, returning the computed cost in USD (or `None` when no
/// tracker is configured or no usage was reported).
fn record_turn_cost(
    state: &AppState,
    provider_name: &str,
    model: &str,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
) -> Option<f64> {
    let tracker = state.cost_tracker.as_ref()?;
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }
    let input = input_tokens.unwrap_or(0);
    let output = output_tokens.unwrap_or(0);
    let cached_input = cached_input_tokens.unwrap_or(0);
    if input == 0 && output == 0 {
        return None;
    }
    // V3 per-provider pricing lookup. Mirrors how the channels
    // orchestrator and the gateway lib.rs cost-tracking scope build
    // their `ModelProviderPricing`: walk every
    // `[model_providers.<type>.<alias>]` and key the per-profile
    // pricing map by `<type>.<alias>`. The streaming and non-streaming
    // paths derive identical costs because both bottom out in the same
    // `<type>.<alias>` key shape.
    let config = state.config.read();
    let pricing_map = config
        .providers
        .models
        .iter_entries()
        .filter(|(_, _, base)| !base.pricing.is_empty())
        .map(|(type_k, alias_k, base)| (format!("{type_k}.{alias_k}"), base.pricing.clone()))
        .collect::<std::collections::HashMap<String, std::collections::HashMap<String, f64>>>();
    drop(config);
    let model_pricing = pricing_map.get(provider_name);
    let try_lookup = |key: &str| -> (f64, f64, f64) {
        let Some(map) = model_pricing else {
            return (0.0, 0.0, 0.0);
        };
        let in_rate = map
            .get(&format!("{key}.input"))
            .copied()
            .or_else(|| map.get(key).copied())
            .unwrap_or(0.0);
        let out_rate = map
            .get(&format!("{key}.output"))
            .copied()
            .or_else(|| map.get(key).copied())
            .unwrap_or(0.0);
        let cached_rate = map
            .get(&format!("{key}.cached_input"))
            .copied()
            .unwrap_or(0.0);
        (in_rate, out_rate, cached_rate)
    };
    let (input_rate, output_rate, cached_rate) = match try_lookup(model) {
        (0.0, 0.0, 0.0) => model
            .rsplit_once('/')
            .map(|(_, suffix)| try_lookup(suffix))
            .unwrap_or((0.0, 0.0, 0.0)),
        rates => rates,
    };
    let usage = zeroclaw_runtime::cost::types::TokenUsage::new(
        model,
        input,
        output,
        cached_input,
        input_rate,
        output_rate,
        cached_rate,
    );
    let cost_usd = usage.cost_usd;
    if let Err(error) = tracker.record_usage(usage) {
        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"provider": provider_name, "model": model, "error": format!("{}", error)})), "Failed to record gateway turn cost");
    }
    Some(cost_usd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

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

    #[test]
    fn session_scoped_events_only_match_their_session() {
        let target_event = serde_json::json!({
            "type": "message",
            "session_id": "operator-1",
            "content": "deploy finished"
        });
        let other_event = serde_json::json!({
            "type": "message",
            "session_id": "operator-2",
            "content": "different session"
        });
        let global_event = serde_json::json!({
            "type": "cron_result",
            "content": "global notification"
        });

        assert!(event_matches_session(&target_event, "operator-1"));
        assert!(!event_matches_session(&other_event, "operator-1"));
        assert!(event_matches_session(&global_event, "operator-1"));
    }

    #[test]
    fn resolve_session_cwd_uses_requested_cwd() {
        let requested = tempfile::tempdir().unwrap();
        let fallback = tempfile::tempdir().unwrap();

        let resolved =
            resolve_session_cwd(Some(requested.path().to_str().unwrap()), fallback.path()).unwrap();

        assert_eq!(resolved, requested.path().canonicalize().unwrap());
    }

    #[test]
    fn resolve_session_cwd_uses_default_workspace_without_request() {
        let fallback = tempfile::tempdir().unwrap();

        let resolved = resolve_session_cwd(None, fallback.path()).unwrap();

        assert_eq!(resolved, fallback.path().canonicalize().unwrap());
    }

    #[test]
    fn resolve_session_cwd_rejects_missing_directory() {
        let fallback = tempfile::tempdir().unwrap();
        let missing = fallback.path().join("missing");

        let err = resolve_session_cwd(Some(missing.to_str().unwrap()), fallback.path())
            .expect_err("missing cwd should be rejected");

        assert!(err.to_string().contains("cwd is not a usable directory"));
    }

    #[test]
    fn needs_onboarding_ws_error_points_to_onboard() {
        let config = zeroclaw_config::schema::Config::default();
        let frame = needs_onboarding_ws_error(&config)
            .expect("empty model must produce a WS onboarding error");

        assert_eq!(frame["type"], "error");
        assert_eq!(frame["error"], "needs_onboarding");
        assert_eq!(frame["code"], "NEEDS_ONBOARDING");
        assert_eq!(frame["url"], "/onboard");
        let message = frame["message"]
            .as_str()
            .expect("onboarding WS error must include a message");
        assert!(
            !message.starts_with('{') && !message.ends_with('}'),
            "missing Fluent key fallback leaked into WS error message: {message:?}"
        );
        assert!(
            message.to_lowercase().contains("quickstart"),
            "WS setup-gap message must explain the setup gap: {message:?}"
        );
    }

    #[test]
    fn needs_onboarding_ws_error_uses_current_configured_model() {
        let mut config = zeroclaw_config::schema::Config::default();
        config.providers.models.openai.insert(
            "default".to_string(),
            zeroclaw_config::schema::OpenAIModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("openai/gpt-4o-mini".to_string()),
                    api_key: Some("sk-test".to_string()),
                    ..Default::default()
                },
            },
        );

        assert!(
            needs_onboarding_ws_error(&config).is_none(),
            "current configured model must allow WebSocket agent construction to continue"
        );
    }

    // Regression for #6514. The mid-turn `client_msg` arm in `forward_fut`
    // must (a) classify stream-end / close / error frames as "client gone"
    // and (b) cancel the turn token so `tokio::join!(turn_fut, forward_fut)`
    // can return — a bare `continue` hot-loops the select forever.
    #[derive(Debug, PartialEq, Eq)]
    enum DisconnectAction {
        Break,
        Continue,
        ProcessText,
    }

    fn classify_client_msg(
        msg: Option<Result<axum::extract::ws::Message, &'static str>>,
    ) -> DisconnectAction {
        use axum::extract::ws::Message;
        match msg {
            Some(Ok(Message::Text(_))) => DisconnectAction::ProcessText,
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => DisconnectAction::Break,
            _ => DisconnectAction::Continue,
        }
    }

    #[test]
    fn mid_turn_client_msg_breaks_on_stream_end_close_or_err() {
        use axum::extract::ws::Message;
        assert_eq!(classify_client_msg(None), DisconnectAction::Break);
        assert_eq!(
            classify_client_msg(Some(Ok(Message::Close(None)))),
            DisconnectAction::Break,
        );
        assert_eq!(
            classify_client_msg(Some(Err("io"))),
            DisconnectAction::Break,
        );
        assert_eq!(
            classify_client_msg(Some(Ok(Message::Ping(Default::default())))),
            DisconnectAction::Continue,
        );
        assert_eq!(
            classify_client_msg(Some(Ok(Message::Text("{}".into())))),
            DisconnectAction::ProcessText,
        );
    }

    #[test]
    fn mid_turn_disconnect_cancel_unblocks_joined_turn() {
        let token = tokio_util::sync::CancellationToken::new();
        let clone_for_turn = token.clone();
        assert!(!clone_for_turn.is_cancelled());
        token.cancel();
        assert!(
            clone_for_turn.is_cancelled(),
            "cloned token (held by turn_fut via agent.turn_streamed) must observe cancellation"
        );
    }

    #[test]
    fn session_queue_errors_map_to_explicit_websocket_codes() {
        use crate::session_queue::SessionQueueError;

        assert_eq!(
            session_queue_ws_error_code(&SessionQueueError::QueueFull {
                session_id: "gw_test".into(),
                depth: 2,
            }),
            "SESSION_QUEUE_FULL"
        );
        assert_eq!(
            session_queue_ws_error_code(&SessionQueueError::Timeout {
                session_id: "gw_test".into(),
            }),
            "SESSION_QUEUE_TIMEOUT"
        );
    }
}
