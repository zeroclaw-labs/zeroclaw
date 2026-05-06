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
use std::path::{Path, PathBuf};
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

    let session_id = params.session_id;
    let session_name = params.name;
    let session_cwd = params.cwd.or(params.workspace_dir);
    ws.on_upgrade(move |socket| handle_socket(socket, state, session_id, session_name, session_cwd))
        .into_response()
}

/// Gateway session key prefix to avoid collisions with channel sessions.
const GW_SESSION_PREFIX: &str = "gw_";

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    session_id: Option<String>,
    session_name: Option<String>,
    session_cwd: Option<String>,
) {
    let (mut sender, mut receiver) = socket.split();

    // Resolve session ID: use provided or generate a new UUID
    let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_key = format!("{GW_SESSION_PREFIX}{session_id}");
    let mut memory_session_id = session_id.clone();

    // Hydrate session metadata from persistence (if available). Agent
    // construction is deferred until after the optional `connect` frame so the
    // client can provide a per-session cwd for the security sandbox root.
    let config = state.config.lock().clone();
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
                        debug!(
                            session_id = ?cp.session_id,
                            device_name = ?cp.device_name,
                            capabilities = ?cp.capabilities,
                            cwd = ?cp.cwd,
                            "WebSocket connect params received"
                        );
                        if let Some(sid) = &cp.session_id {
                            memory_session_id = sid.clone();
                            debug!(
                                session_id = sid,
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

    let session_cwd = match resolve_session_cwd(requested_cwd.as_deref(), &config.workspace_dir) {
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

    // Build a persistent Agent for this connection so history is maintained
    // across turns. The session cwd becomes the security sandbox root; config
    // workspace remains the daemon data directory.
    let mut agent = match zeroclaw_runtime::agent::Agent::from_config_with_session_cwd(
        &config,
        Some(&session_cwd),
    )
    .await
    {
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
    agent.set_memory_session_id(Some(memory_session_id));
    if !stored_messages.is_empty() {
        agent.seed_history(&stored_messages);
    }

    // Process the first message if it was not a connect frame
    if let Some(ref text) = first_msg_fallback {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
            if parsed["type"].as_str() == Some("message") {
                let content = parsed["content"].as_str().unwrap_or("").to_string();
                if !content.is_empty() {
                    // Persist user message
                    if let Some(ref backend) = state.session_backend {
                        let user_msg = zeroclaw_providers::ChatMessage::user(&content);
                        let _ = backend.append(&session_key, &user_msg);
                    }
                    process_chat_message(&state, &mut agent, &mut sender, &content, &session_key)
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
                    let duplex_enabled = state
                        .config
                        .lock()
                        .channels
                        .voice_duplex
                        .as_ref()
                        .is_some_and(|v| v.enabled);
                    if duplex_enabled {
                        if let Some(voice_event) = crate::voice_duplex::try_parse_voice_event(&msg) {
                            if let Some(error_frame) = crate::voice_duplex::handle_voice_event(voice_event) {
                                let _ = sender.send(Message::Text(error_frame.to_string().into())).await;
                            }
                            continue;
                        }
                    }
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
                            "code": "SESSION_BUSY"
                        });
                        let _ = sender.send(Message::Text(err.to_string().into())).await;
                        continue;
                    }
                };

                // Persist user message
                if let Some(ref backend) = state.session_backend {
                    let user_msg = zeroclaw_providers::ChatMessage::user(&content);
                    let _ = backend.append(&session_key, &user_msg);
                }

                process_chat_message(&state, &mut agent, &mut sender, &content, &session_key).await;
            }

            // ── Broadcast event (cron/heartbeat results) ──────────────
            event = broadcast_rx.recv() => {
                if let Ok(event) = event {
                    let _ = sender.send(Message::Text(event.to_string().into())).await;
                }
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
    std::fs::canonicalize(&cwd)
        .map_err(|e| anyhow::anyhow!("cwd is not a usable directory ({}): {e}", cwd.display()))
}

/// Process a single chat message through the agent and send the response.
///
/// Uses [`Agent::turn_streamed`] so that intermediate text chunks, tool calls,
/// and tool results are forwarded to the WebSocket client in real time.
async fn process_chat_message(
    state: &AppState,
    agent: &mut zeroclaw_runtime::agent::Agent,
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    content: &str,
    session_key: &str,
) {
    use zeroclaw_runtime::agent::TurnEvent;

    let provider_label = state
        .config
        .lock()
        .providers
        .fallback
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    // Broadcast agent_start event
    let _ = state.event_tx.send(serde_json::json!({
        "type": "agent_start",
        "provider": provider_label,
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

    // Run the streamed turn concurrently: the agent produces events
    // while we forward them to the WebSocket below.  We cannot move
    // `agent` into a spawned task (it is `&mut`), so we use a join
    // instead — `turn_streamed` writes to the channel and we drain it
    // from the other branch.
    let content_owned = content.to_string();
    let session_key_owned = session_key.to_string();
    let turn_fut = async {
        zeroclaw_runtime::agent::loop_::scope_session_key(
            Some(session_key_owned),
            agent.turn_streamed(&content_owned, event_tx, Some(cancel_token.clone())),
        )
        .await
    };

    // Drive both futures concurrently: the agent turn produces events
    // and we relay them over WebSocket. Track streamed chunks so we
    // can reconstruct partial content on cancellation.
    //
    // WHY incremental persistence: If the process crashes during streaming,
    // the assistant's response is lost — only the user message survives.
    // We append a placeholder assistant message on the first chunk, then
    // update_last periodically (every 500ms) so partial content survives.
    // The final response overwrites this via update_last on completion.
    let mut accumulated_text = String::new();
    let mut partial_saved = false;
    let mut last_partial_save = std::time::Instant::now();
    let partial_save_interval = std::time::Duration::from_millis(500);

    // Aggregate token usage across all LLM calls in this turn.
    // The agent emits TurnEvent::Usage once per LLM call when the provider
    // surfaces usage; here we sum to produce a single done-frame total.
    let mut total_input_tokens: Option<u64> = None;
    let mut total_output_tokens: Option<u64> = None;

    let forward_fut = async {
        while let Some(event) = event_rx.recv().await {
            let ws_msg = match event {
                TurnEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cost_usd: _,
                } => {
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

                    // Incremental persistence: save partial content so it
                    // survives a crash. First chunk appends, subsequent
                    // chunks update in-place.
                    if last_partial_save.elapsed() >= partial_save_interval {
                        if let Some(ref backend) = state.session_backend {
                            let partial =
                                zeroclaw_providers::ChatMessage::assistant(&accumulated_text);
                            if partial_saved {
                                let _ = backend.update_last(session_key, &partial);
                            } else {
                                let _ = backend.append(session_key, &partial);
                                partial_saved = true;
                            }
                        }
                        last_partial_save = std::time::Instant::now();
                    }

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
            };
            let _ = sender.send(Message::Text(ws_msg.to_string().into())).await;
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
        Err(e) => zeroclaw_runtime::agent::loop_::is_tool_loop_cancelled(e),
        Ok(_) => false,
    };

    if was_cancelled {
        // Store partial content with interruption marker so the
        // conversation stays coherent for subsequent turns.
        let truncated = if accumulated_text.is_empty() {
            "[interrupted by user]".to_string()
        } else {
            format!("{accumulated_text}\n\n[interrupted by user]")
        };

        if let Some(ref backend) = state.session_backend {
            let assistant_msg = zeroclaw_providers::ChatMessage::assistant(&truncated);
            if partial_saved {
                let _ = backend.update_last(session_key, &assistant_msg);
            } else {
                let _ = backend.append(session_key, &assistant_msg);
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
            "provider": provider_label,
            "model": state.model,
        }));

        // Trace the cancelled turn so the doctor / replay tool sees it
        // alongside successful turns. #6001 follow-through.
        zeroclaw_runtime::observability::runtime_trace::record_event(
            "gateway_ws_turn",
            Some("ws"),
            Some(&provider_label),
            Some(&state.model),
            Some(&turn_id),
            Some(false),
            Some("interrupted by user"),
            serde_json::json!({ "session_key": session_key, "cancelled": true }),
        );

        return;
    }

    match result {
        Ok(response) => {
            // Persist final assistant response. If we saved partial content
            // during streaming, update it in-place; otherwise append fresh.
            if let Some(ref backend) = state.session_backend {
                let assistant_msg = zeroclaw_providers::ChatMessage::assistant(&response);
                if partial_saved {
                    let _ = backend.update_last(session_key, &assistant_msg);
                } else {
                    let _ = backend.append(session_key, &assistant_msg);
                }
            }

            // Fire-and-forget memory consolidation so facts from WS sessions
            // are extracted to long-term memory (Daily + Core categories).
            if state.auto_save {
                let mem = state.mem.clone();
                let provider = state.provider.clone();
                let model = state.model.clone();
                let user_msg = content.to_string();
                let assistant_resp = response.clone();
                tokio::spawn(async move {
                    if let Err(e) = zeroclaw_memory::consolidation::consolidate_turn(
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

            // Send chunk_reset so the client clears any accumulated draft
            // before the authoritative done message.
            let reset = serde_json::json!({ "type": "chunk_reset" });
            let _ = sender.send(Message::Text(reset.to_string().into())).await;

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
            );

            let done = serde_json::json!({
                "type": "done",
                "full_response": response,
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
                "provider": provider_label,
                "model": state.model,
            }));

            // Append a runtime-trace.jsonl record so a `zeroclaw doctor`
            // sweep sees gateway WS turns alongside channel and CLI turns.
            // Closes the gateway-side trace gap from #6001.
            zeroclaw_runtime::observability::runtime_trace::record_event(
                "gateway_ws_turn",
                Some("ws"),
                Some(&provider_label),
                Some(&state.model),
                Some(&turn_id),
                Some(true),
                None,
                serde_json::json!({
                    "session_key": session_key,
                    "input_tokens": total_input_tokens,
                    "output_tokens": total_output_tokens,
                    "tokens_used": total_tokens,
                    "cost_usd": cost_usd,
                }),
            );
        }
        Err(e) => {
            // Set session state to error
            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "error", Some(&turn_id));
            }

            tracing::error!(error = %e, "Agent turn failed");
            let sanitized = zeroclaw_providers::sanitize_api_error(&e.to_string());
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
            zeroclaw_runtime::observability::runtime_trace::record_event(
                "gateway_ws_turn",
                Some("ws"),
                Some(&provider_label),
                Some(&state.model),
                Some(&turn_id),
                Some(false),
                Some(&sanitized),
                serde_json::json!({ "session_key": session_key, "error_code": error_code }),
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
) -> Option<f64> {
    let tracker = state.cost_tracker.as_ref()?;
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }
    let input = input_tokens.unwrap_or(0);
    let output = output_tokens.unwrap_or(0);
    if input == 0 && output == 0 {
        return None;
    }
    let prices = state.config.lock().cost.prices.clone();
    // 3-tier model pricing lookup mirrors record_tool_loop_cost_usage so
    // streaming and non-streaming paths derive identical costs.
    let pricing = prices
        .get(model)
        .or_else(|| prices.get(&format!("{provider_name}/{model}")))
        .or_else(|| {
            model
                .rsplit_once('/')
                .and_then(|(_, suffix)| prices.get(suffix))
        });
    let usage = zeroclaw_runtime::cost::types::TokenUsage::new(
        model,
        input,
        output,
        pricing.map_or(0.0, |entry| entry.input),
        pricing.map_or(0.0, |entry| entry.output),
    );
    let cost_usd = usage.cost_usd;
    if let Err(error) = tracker.record_usage(usage) {
        tracing::warn!(
            provider = provider_name,
            model,
            "Failed to record gateway turn cost: {error}"
        );
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
}
