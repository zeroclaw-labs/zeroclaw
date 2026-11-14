//! WebSocket agent chat handler.
//!
//! Approval summaries are operator-facing strings produced by the runtime's
//! key-name redaction heuristic. Approval decisions bind to `request_id`; this
//! transport forwards the summary without rebuilding it from raw arguments.

use super::AppState;
use crate::ws_approval::{PendingApprovals, WsApprovalChannel};
use crate::ws_conversation::{Conversation, FrameSink, Seed, Submitted, TurnClaim};
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
use zeroclaw_api::chat_surface::ChatSurface;
use zeroclaw_infra::session_backend::RequestReceipt;

mod device;
pub(crate) mod intake;
mod surface;
pub(crate) mod turn_control;

/// Default wall-clock budget for the operator to answer an
/// `approval_request` frame before the channel auto-denies. Mirrors the
/// channel-side default on `TelegramConfig::approval_timeout_secs`.
const WS_APPROVAL_TIMEOUT_SECS: u64 = 120;

/// Single ingress identity for gateway WebSocket turns.
///
/// This name is used in three places that MUST agree:
///   1. `Agent.channel_name` — observer/attribution events for the turn
///   2. the turn span's `channel` field — tracing/log correlation
///   3. the interactive back-channel registration key — how `ask_user`,
///      `poll`, and `escalate_to_human` find this conversation
///
/// If (3) diverges from (1) and (2), one turn is split across two channel
/// names in observability while interactive tools still route correctly —
/// or, worse, tools route to an arbitrary seeded channel.
const WS_CHANNEL_KEY: &str = "wss";

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
    #[serde(default)]
    surface: Option<ChatSurface>,
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
    pub surface: Option<String>,
}

pub(crate) fn extract_ws_token<'a>(
    headers: &'a HeaderMap,
    query_token: Option<&'a str>,
) -> Option<&'a str> {
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

fn client_offers_chat_protocol(headers: &HeaderMap) -> bool {
    headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == WS_PROTOCOL))
}

/// GET /ws/chat — WebSocket upgrade for agent chat
pub async fn handle_ws_chat(
    State(state): State<AppState>,
    Query(params): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if !device::authenticated_mode(&state, &state.config.read()) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(device::refusal("BRIDGE_BINDING_REJECTED")),
        )
            .into_response();
    }
    // Auth: check header, subprotocol, then query param (precedence order). On
    // success derive a STABLE transport-authenticated subject (the paired-token
    // hash) so a required-group approval policy can be satisfied over WS; an
    // operator grants approval rights to this paired device via a `ws:<token-hash>`
    // group member. Pairing-off anonymous sockets have no auth identity.
    //
    // A bridge token (`[gateway.bridges.<name>]`) also authenticates, but only
    // for the sessions its entry scopes it to. Its subject is its token hash,
    // like a paired token's.
    let presented = extract_ws_token(&headers, params.token.as_deref()).unwrap_or("");
    let bridge_scope = state
        .config
        .read()
        .gateway
        .bridge_for_token(presented)
        .map(|(name, bridge)| (name.to_string(), bridge.clone()));
    let auth_subject = if let Some((bridge, scope)) = &bridge_scope {
        let allowed = params
            .session_id
            .as_deref()
            .is_some_and(|session| scope.allows_session(session));
        if !allowed {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "bridge": bridge,
                        "session_id": params.session_id,
                    })),
                "bridge token used outside its session scope"
            );
            return (
                axum::http::StatusCode::FORBIDDEN,
                "Forbidden: this bridge token may not open that session (see [gateway.bridges.<name>] sessions / session_prefix)",
            )
                .into_response();
        }
        Some(zeroclaw_config::tokens::hash_token(presented))
    } else if state.pairing.require_pairing() {
        match state.pairing.authenticate_and_hash(presented) {
            Some(hash) => Some(hash),
            None => {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    "Unauthorized: provide Authorization header, Sec-WebSocket-Protocol bearer, or ?token= query param",
                )
                    .into_response();
            }
        }
    } else {
        // Pairing-off permits anonymous chat, not anonymous answers. The
        // guard's authenticate_and_hash deliberately allows everyone in
        // that mode, so check actual stored device membership instead.
        let hash = zeroclaw_config::tokens::hash_token(presented);
        (!presented.is_empty() && state.pairing.tokens().contains(&hash)).then_some(hash)
    };
    let bridge_scope = bridge_scope.map(|(name, initial)| device::Binding { name, initial });

    // Echo Sec-WebSocket-Protocol if the client requests our sub-protocol.
    // Absence is allowed: `/ws/chat` is not fail-closed on a missing token.
    // `/ws/nodes` v2 is the opposite — it rejects a missing or v1-only offer.
    let ws = if client_offers_chat_protocol(&headers) {
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

    if let Some(binding) = &bridge_scope
        && (!params.session_id.as_deref().is_some_and(|session| {
            binding.allows(&state.config.read(), &agent_alias, session, "chat")
        }) || (binding.restricted()
            && (params.cwd.is_some() || params.workspace_dir.is_some())))
    {
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(device::refusal("BRIDGE_BINDING_REJECTED")),
        )
            .into_response();
    }

    let surface = match surface::resolve(None, params.surface.as_deref()) {
        Ok(surface) => surface,
        Err(()) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                zeroclaw_runtime::i18n::get_required_cli_string("gateway-invalid-surface"),
            )
                .into_response();
        }
    };
    let session_id = params.session_id;
    let session_name = params.name;
    let session_cwd = params.cwd.or(params.workspace_dir);
    let shutdown = state.shutdown_tx.subscribe();
    ws.on_upgrade(move |socket| async move {
        // Cover the entire lifetime, including a client that never sends its
        // connect frame. Dropping a socket only detaches; owned turns keep the
        // hub/lease until they actually finish.
        tokio::select! {
            biased;
            () = crate::gateway_shutdown(shutdown) => {},
            () = handle_socket(
            socket,
            state,
            agent_alias,
            session_id,
            session_name,
            session_cwd,
            surface,
            auth_subject,
            bridge_scope,
            ) => {},
        }
    })
    .into_response()
}

/// Gateway session key prefix to avoid collisions with channel sessions.
const GW_SESSION_PREFIX: &str = "gw_";

async fn resolve_ws_memory_handle(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> anyhow::Result<Option<Arc<dyn zeroclaw_memory::Memory>>> {
    if config.agent(agent_alias).is_some_and(|agent| {
        matches!(
            agent.memory.backend,
            zeroclaw_config::multi_agent::MemoryBackendKind::None
        )
    }) {
        return Ok(None);
    }

    let api_key = config
        .resolved_model_provider_for_agent(agent_alias)
        .and_then(|(_, _, cfg)| cfg.api_key.clone());
    zeroclaw_memory::create_memory_for_agent(config, agent_alias, api_key.as_deref())
        .await
        .map(Some)
}

#[allow(clippy::too_many_arguments)]
async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    agent_alias: String,
    session_id: Option<String>,
    session_name: Option<String>,
    session_cwd: Option<String>,
    mut surface: Option<ChatSurface>,
    // The transport-authenticated approval subject (paired-token hash), if the
    // connection was authenticated. Threaded to SOP approval frames so a policied
    // gate can be satisfied by an identified WS caller.
    auth_subject: Option<String>,
    // Set when a bridge token opened the socket: the sessions it may use.
    bridge_scope: Option<device::Binding>,
) {
    let (mut sender, mut receiver) = socket.split();

    // Resolve session ID: use provided or generate a new UUID
    let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_key = format!("{GW_SESSION_PREFIX}{session_id}");
    let lifecycle = state.ws_conversations.session_lifecycle(&session_key);
    // Match the sanitized form persisted by memory backend migrations.
    let mut memory_session_id = zeroclaw_api::session_keys::sanitize_session_key(&session_id);

    let restricted = bridge_scope
        .as_ref()
        .is_some_and(device::Binding::restricted);
    if let Some(binding) = &bridge_scope {
        let alias_matches = !restricted
            || state.session_backend.as_ref().is_none_or(|backend| {
                backend
                    .get_session_agent_alias(&session_key)
                    .is_ok_and(|stored| stored.is_none_or(|alias| alias == agent_alias))
            });
        if !alias_matches
            || !binding.allows(&state.config.read(), &agent_alias, &session_id, "chat")
        {
            let _ = sender
                .send(Message::Text(
                    device::refusal("BRIDGE_BINDING_REJECTED")
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    }

    // Hydrate session metadata from persistence (if available). Agent
    // construction is deferred until after the optional `connect` frame so the
    // client can provide a per-session cwd for the security sandbox root.
    let config = state.config.read().clone();
    let mut resumed = false;
    let mut message_count: usize = 0;
    let mut effective_name: Option<String> = None;
    {
        let _lifecycle = lifecycle.lock().await;
        if let Some(ref backend) = state.session_backend {
            let messages = backend.load(&session_key);
            if !messages.is_empty() {
                message_count = messages.len();
                resumed = true;
            }
            if !restricted {
                effective_name = stamp_session(
                    backend.as_ref(),
                    &session_key,
                    &agent_alias,
                    session_name.as_deref(),
                );
            }
        }
    }

    // Send session_start message to client
    let mut session_start = serde_json::json!({
        "type": "session_start",
        "session_id": session_id,
        "resumed": resumed,
        "message_count": message_count,
        "surface_version": surface::VERSION,
        "surface": surface,
    });
    if let Some(ref name) = effective_name {
        session_start["name"] = serde_json::Value::String(name.clone());
    }
    let _ = sender
        .send(Message::Text(session_start.to_string().into()))
        .await;

    let mut first_msg_fallback: Option<String> = None;
    let mut requested_cwd = session_cwd;

    if let Some(first) = receiver.next().await {
        match first {
            Ok(Message::Text(text)) => {
                if restricted
                    && let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text)
                    && frame["type"] == "connect"
                    && (!device::connect_matches(&frame, &agent_alias, &session_id)
                        || serde_json::from_value::<ConnectParams>(frame).is_err())
                {
                    let _ = sender
                        .send(Message::Text(
                            device::refusal("BRIDGE_BINDING_REJECTED")
                                .to_string()
                                .into(),
                        ))
                        .await;
                    return;
                }
                if let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text)
                    && frame["type"] == "connect"
                    && surface::connect_value(surface, &frame).is_err()
                {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": zeroclaw_runtime::i18n::get_required_cli_string("gateway-invalid-surface"),
                        "code": "INVALID_SURFACE"
                    });
                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                    return;
                }
                if let Ok(cp) = serde_json::from_str::<ConnectParams>(&text) {
                    if cp.msg_type == "connect" {
                        surface = cp.surface.or(surface);
                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"session_id": cp.session_id, "device_name": cp.device_name, "capabilities": cp.capabilities, "cwd": cp.cwd})), "WebSocket connect params received");
                        if let (Some(sid), Some(scope)) = (&cp.session_id, &bridge_scope)
                            && !scope.initial.allows_session(sid)
                        {
                            let err = serde_json::json!({
                                "type": "error",
                                "message": "this bridge token may not use that session",
                                "code": "SESSION_OUT_OF_SCOPE"
                            });
                            let _ = sender.send(Message::Text(err.to_string().into())).await;
                            return;
                        }
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
                            "owner_bridge_cancel": bridge_scope.as_ref().is_some_and(|binding|
                                binding.initial.owner.is_some() && binding.allows(&state.config.read(), &agent_alias, &session_id, "cancel")),
                            "owner_bridge_approval_once": bridge_scope.as_ref().is_some_and(|binding|
                                binding.initial.owner.is_some() && binding.allows(&state.config.read(), &agent_alias, &session_id, "approval_response")),
                            "message": "Connection established",
                            "surface_version": surface::VERSION,
                            "surface": surface,
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

    // Recheck after the asynchronous handshake, before stamping or constructing an Agent.
    if let Some(binding) = &bridge_scope
        && !binding.allows(&state.config.read(), &agent_alias, &session_id, "chat")
    {
        let _ = sender
            .send(Message::Text(
                device::refusal("BRIDGE_BINDING_REJECTED")
                    .to_string()
                    .into(),
            ))
            .await;
        return;
    }
    let session_cwd = match resolve_ws_session_cwd(requested_cwd.as_deref(), &config, &agent_alias)
    {
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

    // Every socket that opens this session with this agent shares one
    // conversation: one agent, one history, one running turn (#376).
    // The client handshake must not hold the lifecycle guard. Re-read history
    // under it so DELETE cannot be undone by a pre-deletion history snapshot.
    let lifecycle_guard = lifecycle.lock().await;
    if let Some(backend) = &state.session_backend {
        stamp_session(
            backend.as_ref(),
            &session_key,
            &agent_alias,
            if restricted {
                None
            } else {
                session_name.as_deref()
            },
        );
    }
    let stored_messages = state
        .session_backend
        .as_ref()
        .map(|backend| backend.load(&session_key))
        .unwrap_or_default();
    let conversation_key = format!("{session_key}\u{1f}{agent_alias}");
    let attached = state
        .ws_conversations
        .attach(&conversation_key, |seed| {
            build_ws_session(
                &config,
                &state,
                &agent_alias,
                &session_key,
                &session_cwd,
                memory_session_id.clone(),
                &stored_messages,
                seed,
            )
        })
        .await;
    drop(lifecycle_guard);
    let (mut subscription, restore_trim_event) = match attached {
        Ok(attached) => attached,
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

    // Restoring history trims it at most once, when the conversation is
    // built. Only the socket that built it is told; later sockets join a
    // conversation whose history is already live.
    if let Some(Some(zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
        dropped_messages,
        kept_turns,
        reason,
    })) = restore_trim_event
    {
        let leak_detection = state.config.read().security.leak_detection.clone();
        let frame =
            history_trimmed_ws_frame(dropped_messages, kept_turns, &reason, &leak_detection);
        let _ = sender.send(Message::Text(frame.to_string().into())).await;
    }

    let approval_frames = subscription
        .conversation
        .approval_frames(&state.config.read().security.leak_detection);
    for frame in approval_frames {
        if frame.is_current(subscription.conversation.reset_revision()) {
            let _ = sender.send(Message::Text(frame.to_string().into())).await;
        }
    }

    let question_frames = subscription
        .conversation
        .questions
        .frames(&state.config.read());
    for frame in question_frames {
        let _ = sender.send(Message::Text(frame.to_string().into())).await;
    }

    let scope = WsTurnScope {
        session_key,
        session_id,
        auth_subject,
        surface,
        bridge: bridge_scope,
    };

    // Subscribe to the shared broadcast channel so events addressed to this
    // session (e.g. messages appended through the sessions API) reach this
    // client, during turns as well.
    let mut broadcast_rx = state.event_tx.subscribe();
    let mut next_text = first_msg_fallback;
    let mut turn_negotiated = false;
    let mut steering_negotiated = false;

    loop {
        let text = match next_text.take() {
            Some(text) => text,
            None => tokio::select! {
                // ── Client message ────────────────────────────────────
                client_msg = receiver.next() => match client_msg {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    // Closing a socket only unsubscribes; a running turn
                    // keeps going for the other sockets and the history.
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                },

                // ── Conversation frame (turn output, approvals) ───────
                frame = subscription.frames.recv() => {
                    match frame {
                        Ok(frame) => {
                            let current = {
                                let _lifecycle = lifecycle.lock().await;
                                frame.is_current(subscription.conversation.reset_revision())
                            };
                            if !current { continue; }
                            if !device::permitted(&state, &scope, &agent_alias, "chat") { break; }
                            if turn_negotiated && !serde_json::from_str::<serde_json::Value>(&frame).is_ok_and(|frame| {
                                matches!(frame["type"].as_str(), Some("approval_request" | "question" | "answer_ack" | "btw_response" | "btw_error"))
                            }) { continue; }
                            if turn_negotiated && !question_answer_authorized(&state, &scope) { break; }
                            if !turn_control::send(&mut sender, Message::Text(surface::wire(&state, scope.surface, &frame).into()), turn_negotiated).await {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            if turn_negotiated {
                                let _ = turn_control::send(&mut sender, Message::Text(turn_control::gap().to_string().into()), true).await;
                                break;
                            }
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_attrs(::serde_json::json!({
                                        "session_key": scope.session_key,
                                        "skipped": skipped,
                                    })),
                                "WS subscriber fell behind; frames skipped"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                    continue;
                }

                // Negotiated output cannot consume the legacy ring's capacity.
                frame = subscription.control_frames.recv(), if turn_negotiated => {
                    if !device::permitted(&state, &scope, &agent_alias, "chat")
                        || !question_answer_authorized(&state, &scope) { break; }
                    match frame {
                        Ok(frame) => {
                            let visible = {
                                let _lifecycle = lifecycle.lock().await;
                                frame.is_current(subscription.conversation.reset_revision())
                                    && serde_json::from_str::<serde_json::Value>(&frame).is_ok_and(|frame| {
                                        state.ws_conversations.turn_control.visible(subscription.conversation.key(), scope.auth_subject.as_deref(), &frame)
                                    })
                            };
                            if !visible { continue; }
                            if !turn_control::send(&mut sender, Message::Text(surface::wire(&state, scope.surface, &frame).into()), true).await { break; }
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let _ = turn_control::send(&mut sender, Message::Text(turn_control::gap().to_string().into()), true).await;
                            break;
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                    continue;
                }

                // ── Broadcast event addressed to this session ─────────
                event = broadcast_rx.recv() => {
                    if !device::permitted(&state, &scope, &agent_alias, "chat") { break; }
                    if let Ok(event) = event
                        && event_matches_session(&event, &scope.session_id)
                        && !is_observability_telemetry(&event)
                        && (!turn_negotiated || !matches!(event["type"].as_str(), Some("chunk" | "done" | "aborted" | "error")))
                    {
                        if turn_negotiated && !question_answer_authorized(&state, &scope) { break; }
                        if !turn_control::send(&mut sender, Message::Text(surface::wire(&state, scope.surface, &event.to_string()).into()), turn_negotiated).await { break; }
                    }
                    continue;
                }
            },
        };

        if !device::permitted(&state, &scope, &agent_alias, "chat") {
            let _ = turn_control::send(
                &mut sender,
                Message::Text(
                    device::refusal("BRIDGE_OPERATION_DENIED")
                        .to_string()
                        .into(),
                ),
                turn_negotiated,
            )
            .await;
            break;
        }

        // ── Voice duplex event dispatch (gated by feature flag + runtime config) ──
        #[cfg(feature = "gateway-voice-duplex")]
        {
            // Multi-instance shape: presence in the map = enabled.
            let duplex_enabled = !state.config.read().channels.voice_duplex.is_empty();
            if !turn_negotiated
                && duplex_enabled
                && device::permitted(&state, &scope, &agent_alias, "voice")
            {
                if let Some(voice_event) = crate::voice_duplex::try_parse_voice_event(&text) {
                    if let Some(error_frame) = crate::voice_duplex::handle_voice_event(voice_event)
                    {
                        let _ = sender
                            .send(Message::Text(error_frame.to_string().into()))
                            .await;
                    }
                    continue;
                }
            }
        }

        let extension = turn_negotiated
            || serde_json::from_str::<serde_json::Value>(&text)
                .is_ok_and(|frame| frame["type"] == "turn_control");
        let lifecycle_guard = lifecycle.lock().await;
        let (reply, start) = if extension {
            turn_control::handle(
                &state,
                &subscription.conversation,
                &scope,
                &mut turn_negotiated,
                &mut steering_negotiated,
                &text,
            )
        } else {
            handle_client_text(&state, &subscription.conversation, &scope, &text)
        };
        drop(lifecycle_guard);
        // Acceptance is independent of delivering its ACK. Even a failed/slow
        // writer must release the admitted claim to execution and recovery.
        // Handoff has no await: shutdown may drop this socket at the very next
        // await, but must never drop an accepted claim before its task owns it.
        if let Some(claim) = start {
            start_ws_turns(&state, &subscription.conversation, &scope, claim);
        }
        let sent = match reply {
            Some(reply) => {
                turn_control::send(
                    &mut sender,
                    Message::Text(surface::wire(&state, scope.surface, &reply.to_string()).into()),
                    turn_negotiated,
                )
                .await
            }
            None => true,
        };
        if turn_negotiated && !sent {
            break;
        }
    }
}

/// The agent state one shared WS conversation holds.
pub struct WsSession {
    agent: zeroclaw_runtime::agent::Agent,
    /// Per-agent memory for turn-end consolidation; `None` disables it.
    ws_memory: Option<Arc<dyn zeroclaw_memory::Memory>>,
}

impl WsSession {
    pub(crate) fn clear_history(&mut self) {
        self.agent.clear_history();
    }
}

impl crate::ws_conversation::ConversationAgent for WsSession {
    fn side_query_handle(&self) -> Option<zeroclaw_runtime::agent::SideQueryHandle> {
        Some(self.agent.side_query_handle())
    }
}

/// Identifies the session a turn runs for. Cloned into each turn task.
#[derive(Clone)]
struct WsTurnScope {
    session_key: String,
    session_id: String,
    // The transport-authenticated approval subject (paired-token hash) of
    // the socket that opened the conversation, if it was authenticated.
    auth_subject: Option<String>,
    surface: Option<ChatSurface>,
    bridge: Option<device::Binding>,
}

/// Build the agent for a new shared conversation and wire its approval
/// prompts to the conversation's subscribers. Also returns the history-trim
/// event from restoring the stored messages, if restoring trimmed them.
#[allow(clippy::too_many_arguments)]
async fn build_ws_session(
    config: &zeroclaw_config::schema::Config,
    state: &AppState,
    agent_alias: &str,
    _session_key: &str,
    session_cwd: &Path,
    memory_session_id: String,
    stored_messages: &[zeroclaw_providers::ChatMessage],
    seed: Seed,
) -> anyhow::Result<(WsSession, Option<zeroclaw_api::agent::TurnEvent>)> {
    let ws_memory = match resolve_ws_memory_handle(config, agent_alias).await {
        Ok(memory) => memory,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "agent": agent_alias,
                        "error": format!("{e:#}"),
                    })),
                "WS per-agent memory resolution failed; consolidation disabled for session"
            );
            None
        }
    };

    let mut agent =
        zeroclaw_runtime::agent::Agent::from_live_config_with_session_cwd_and_mcp_backchannel(
            Arc::clone(&state.config),
            agent_alias,
            Some(session_cwd),
            true,
            false,
        )
        .await?;
    agent.add_prompt_section(Box::new(surface::Section));
    // Keep ONE ingress identity for the WebSocket turn: the turn span records
    // `channel = "wss"`, and observer events derive from `Agent.channel_name`,
    // so this must stay `wss` or a single turn is split across two names.
    // The back-channel is registered under the same `wss` key below, which is
    // what lets ask_user/poll/escalate_to_human default to this conversation.
    agent.set_channel_name(WS_CHANNEL_KEY.to_string());
    agent.set_memory_session_id(Some(memory_session_id));
    let restore_trim_event = if stored_messages.is_empty() {
        None
    } else {
        agent.seed_history_with_event(stored_messages)
    };

    let Seed {
        pending_approvals,
        questions,
        frames,
    } = seed;
    let (approval_event_tx, approval_event_rx) =
        tokio::sync::mpsc::channel::<zeroclaw_api::agent::TurnEvent>(8);
    let approval_channel = Arc::new(
        WsApprovalChannel::new(
            approval_event_tx,
            pending_approvals.clone(),
            Duration::from_secs(WS_APPROVAL_TIMEOUT_SECS),
        )
        .with_questions(super::ws_question::QuestionPort {
            questions,
            frames: frames.clone(),
            config: Arc::clone(&state.config),
        }),
    );
    agent
        .channel_handles()
        .register_channel(WS_CHANNEL_KEY, approval_channel);
    // Ends when the agent, and with it the approval channel, is dropped.
    let approval_leak_detection = state.config.read().security.leak_detection.clone();
    zeroclaw_spawn::spawn!(relay_approval_requests(
        approval_event_rx,
        frames,
        pending_approvals,
        approval_leak_detection
    ));

    Ok((WsSession { agent, ws_memory }, restore_trim_event))
}

/// Publish prompts to current subscribers. Offline requests remain in the
/// conversation until their original deadline, so reconnect can project them.
async fn relay_approval_requests(
    mut events: tokio::sync::mpsc::Receiver<zeroclaw_api::agent::TurnEvent>,
    frames: FrameSink,
    pending_approvals: PendingApprovals,
    leak_detection: zeroclaw_config::schema::LeakDetectionConfig,
) {
    while let Some(event) = events.recv().await {
        // Forward the runtime-produced summary without inspecting or
        // reconstructing it from the raw argument object.
        let zeroclaw_api::agent::TurnEvent::ApprovalRequest {
            request_id,
            tool_name,
            arguments_summary,
            timeout_secs: _,
        } = event
        else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"kind": format!("{:?}", event)})),
                "non-ApprovalRequest event leaked into approval channel"
            );
            continue;
        };
        // Reset/timeout can revoke the request before this relay is scheduled.
        // Hold the map guard through publication so deletion either invalidates
        // its frame revision or removes the request before we publish it.
        let pending = pending_approvals.lock();
        let Some(timeout_secs) = pending
            .get(&request_id)
            .and_then(|entry| entry.remaining_secs())
        else {
            continue;
        };
        frames.publish(&approval_request_ws_frame(
            &request_id,
            &tool_name,
            &arguments_summary,
            timeout_secs,
            &leak_detection,
        ));
    }
}

/// Leak-only outbound redaction for turn-event frame payloads. Unlike
/// `sanitize_outbound_response` this strips nothing: thinking deltas, tool
/// arguments/results, approval summaries, plan entries and trim reasons must
/// keep their shape, only credential-shaped values are masked.
pub(super) fn redact_frame_text(
    text: &str,
    leak_detection: &zeroclaw_config::schema::LeakDetectionConfig,
) -> String {
    zeroclaw_runtime::security::outbound::redact_channel_outbound_leaks(
        text,
        leak_detection,
        zeroclaw_runtime::security::outbound::OutboundContentFormat::Markdown,
    )
}

/// `redact_frame_text` over a JSON value: structure is preserved, every
/// string inside it is redacted (tool arguments arrive as structured JSON).
fn redact_frame_value(
    value: serde_json::Value,
    leak_detection: &zeroclaw_config::schema::LeakDetectionConfig,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(redact_frame_text(&text, leak_detection))
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| redact_frame_value(item, leak_detection))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, redact_frame_value(item, leak_detection)))
                .collect(),
        ),
        other => other,
    }
}

/// The non-streaming turn-event wire frames: thinking deltas, tool calls and
/// results, approval prompts (belt-and-suspenders — the approval relay is the
/// usual path), history trims, and plan updates. Every human-readable text
/// field is redacted for credential leaks, matching the chunk and done
/// frames; `Usage` and `Chunk` stay in the turn loop (accumulation and the
/// stream redactor are stateful).
fn turn_event_ws_frame(
    event: zeroclaw_api::agent::TurnEvent,
    leak_detection: &zeroclaw_config::schema::LeakDetectionConfig,
) -> serde_json::Value {
    match event {
        zeroclaw_api::agent::TurnEvent::Thinking { delta } => serde_json::json!({
            "type": "thinking",
            "content": redact_frame_text(&delta, leak_detection),
        }),
        zeroclaw_api::agent::TurnEvent::ToolCall { id, name, args } => serde_json::json!({
            "type": "tool_call",
            "id": id,
            "name": name,
            "args": redact_frame_value(args, leak_detection),
        }),
        zeroclaw_api::agent::TurnEvent::ToolResult { id, name, output } => serde_json::json!({
            "type": "tool_result",
            "id": id,
            "name": name,
            "output": redact_frame_text(&output, leak_detection),
        }),
        zeroclaw_api::agent::TurnEvent::ApprovalRequest {
            request_id,
            tool_name,
            arguments_summary,
            timeout_secs,
        } => approval_request_ws_frame(
            &request_id,
            &tool_name,
            &arguments_summary,
            timeout_secs,
            leak_detection,
        ),
        zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
            dropped_messages,
            kept_turns,
            reason,
        } => history_trimmed_ws_frame(dropped_messages, kept_turns, &reason, leak_detection),
        zeroclaw_api::agent::TurnEvent::Plan { entries } => {
            let entries: Vec<_> = entries
                .into_iter()
                .map(|mut entry| {
                    entry.content = redact_frame_text(&entry.content, leak_detection);
                    if let Some(active) = entry.active_form.take() {
                        entry.active_form = Some(redact_frame_text(&active, leak_detection));
                    }
                    entry
                })
                .collect();
            serde_json::json!({ "type": "plan", "entries": entries })
        }
        // Usage and Chunk never reach this helper; the turn loop handles them
        // before falling through to `other`.
        zeroclaw_api::agent::TurnEvent::Usage { .. }
        | zeroclaw_api::agent::TurnEvent::Chunk { .. } => unreachable!(
            "Usage and Chunk events are handled by the turn loop before turn_event_ws_frame"
        ),
    }
}

pub(super) fn approval_request_ws_frame(
    request_id: &str,
    tool_name: &str,
    arguments_summary: &str,
    timeout_secs: u64,
    leak_detection: &zeroclaw_config::schema::LeakDetectionConfig,
) -> serde_json::Value {
    let arguments_summary = redact_frame_text(arguments_summary, leak_detection);
    serde_json::json!({
        "type": "approval_request",
        "request_id": request_id,
        "tool": tool_name,
        "arguments_summary": arguments_summary,
        "timeout_secs": timeout_secs,
    })
}

/// Act on one client text frame. Returns the reply for this socket only (an
/// `ack` or an error) and, when the frame starts a turn, the claim to run
/// it with; everything else reaches the client through the conversation.
fn handle_client_text(
    state: &AppState,
    conversation: &Arc<Conversation<WsSession>>,
    scope: &WsTurnScope,
    text: &str,
) -> (Option<serde_json::Value>, Option<TurnClaim>) {
    let error = |message: String, code: &str| {
        (
            Some(serde_json::json!({ "type": "error", "message": message, "code": code })),
            None,
        )
    };
    let parsed: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return error(format!("Invalid JSON: {e}"), "INVALID_JSON"),
    };
    let alias = conversation
        .key()
        .rsplit_once('\u{1f}')
        .map(|(_, alias)| alias)
        .unwrap_or("");
    if !device::permitted(state, scope, alias, device::frame_operation(&parsed)) {
        return (Some(device::refusal("BRIDGE_OPERATION_DENIED")), None);
    }
    match parsed["type"].as_str().unwrap_or("") {
        "source_resume" => intake::resume(state, conversation, scope, &parsed),
        "source_disposition" => intake::receive(state, conversation, scope, &parsed),
        "answer" => {
            let id = parsed["request_id"].as_str().unwrap_or("");
            let status = if !question_answer_authorized(state, scope) {
                "unauthorized"
            } else if id.is_empty() || id.len() > 128 {
                "invalid"
            } else if let Some(text) = parsed["text"].as_str() {
                conversation.questions.answer(id, text)
            } else {
                "invalid"
            };
            if status == "accepted" {
                conversation.publish(
                    &serde_json::json!({"type":"answer_ack", "request_id":id, "status":status}),
                );
                (None, None)
            } else {
                (
                    Some(
                        serde_json::json!({"type":"answer_ack", "request_id":id, "status":status}),
                    ),
                    None,
                )
            }
        }
        // ── approval_response (operator answered a tool prompt) ──
        "approval_response" => {
            let request_id = parsed["request_id"].as_str().unwrap_or("");
            let decision = match parsed["decision"].as_str().unwrap_or("") {
                "approve" => ChannelApprovalResponse::Approve,
                "always" => ChannelApprovalResponse::AlwaysApprove,
                "deny" => ChannelApprovalResponse::Deny,
                _ => {
                    return error(
                        "approval_response requires request_id and decision in {approve,deny,always}".into(),
                        "INVALID_APPROVAL_RESPONSE",
                    );
                }
            };
            if request_id.is_empty() {
                return error(
                    "approval_response requires request_id and decision in {approve,deny,always}"
                        .into(),
                    "INVALID_APPROVAL_RESPONSE",
                );
            }
            // Only an operation-authorized subscriber may answer; first answer counts.
            if !conversation.resolve_approval(request_id, decision) {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"request_id": request_id})),
                    "approval_response with no matching pending request"
                );
            }
            (None, None)
        }
        // ── cancel (stop the running turn for every subscriber) ──
        "cancel"
            if parsed.get("control_version").is_some()
                || parsed.get("turn_id").is_some()
                || parsed.get("id").is_some() =>
        {
            cancel_control(state, conversation, scope, &parsed)
        }
        "cancel" => {
            if conversation.cancel_current() {
                (None, None)
            } else {
                error("No turn is running".into(), "NO_ACTIVE_TURN")
            }
        }
        "message" => handle_message_frame(state, conversation, scope, &parsed),
        "btw" => handle_btw_frame(state, conversation, scope, &parsed),
        other => error(
            format!(
                "Unsupported message type \"{other}\". Send {{\"type\":\"message\",\"content\":\"your text\"}}"
            ),
            "UNKNOWN_MESSAGE_TYPE",
        ),
    }
}

/// The additive v1 operation never falls back to cancelling whichever turn is
/// current. Its acknowledgement reports a request, not completed cancellation.
fn cancel_control(
    state: &AppState,
    conversation: &Conversation<WsSession>,
    scope: &WsTurnScope,
    frame: &serde_json::Value,
) -> (Option<serde_json::Value>, Option<TurnClaim>) {
    let id = frame["id"].as_str().unwrap_or("");
    let target = frame["turn_id"].as_str().unwrap_or("");
    let status = if frame["control_version"].as_u64() != Some(1)
        || id.is_empty()
        || id.len() > MAX_REQUEST_ID_LEN
        || id.chars().any(char::is_control)
        || uuid::Uuid::parse_str(target).is_err()
    {
        "invalid"
    } else if !question_answer_authorized(state, scope)
        || !conversation
            .key()
            .rsplit_once('\u{1f}')
            .is_some_and(|(_, alias)| {
                state
                    .config
                    .read()
                    .agents
                    .get(alias)
                    .is_some_and(|agent| agent.enabled)
            })
    {
        "unauthorized"
    } else {
        conversation.cancel_turn(target)
    };
    let key = format!("gateway-cancel-{}", status.replace('_', "-"));
    (
        Some(serde_json::json!({
            "type":"cancel_ack", "control_version":1, "id":id, "turn_id":target,
            "status":status,
            "message":zeroclaw_runtime::i18n::get_required_cli_string(&key),
        })),
        None,
    )
}

/// Resolve current device/bridge authority at answer time, including revocation.
fn question_answer_authorized(state: &AppState, scope: &WsTurnScope) -> bool {
    let Some(subject) = &scope.auth_subject else {
        return false;
    };
    if state.pairing.tokens().contains(subject) {
        return true;
    }
    let config = state.config.read();
    config.gateway.bridges.values().any(|bridge| {
        bridge.allows_session(&scope.session_id)
            && zeroclaw_config::tokens::constant_time_eq(
                subject,
                &bridge.token_hash.to_ascii_lowercase(),
            )
    })
}

/// The longest client request id accepted on a `message` frame.
const MAX_REQUEST_ID_LEN: usize = 128;

/// Accept a `message` frame. With an `id`, the request is recorded before
/// anything runs and answered with an `ack`; a repeated `id` is answered
/// with its recorded state and not run again.
fn handle_message_frame(
    state: &AppState,
    conversation: &Arc<Conversation<WsSession>>,
    scope: &WsTurnScope,
    parsed: &serde_json::Value,
) -> (Option<serde_json::Value>, Option<TurnClaim>) {
    if parsed.get("source").is_some() {
        return intake::receive(state, conversation, scope, parsed);
    }
    let request_id = match parsed.get("id") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(id))
            if !id.is_empty()
                && id.len() <= MAX_REQUEST_ID_LEN
                && !id.chars().any(char::is_control) =>
        {
            Some(id.as_str())
        }
        Some(_) => {
            return (
                Some(serde_json::json!({
                    "type": "error",
                    "message": format!(
                        "message id must be a non-empty string of at most {MAX_REQUEST_ID_LEN} characters without control characters"
                    ),
                    "code": "INVALID_REQUEST_ID",
                })),
                None,
            );
        }
    };
    let reject = |message: &str, code: &str| {
        let mut frame = serde_json::json!({ "type": "error", "message": message, "code": code });
        stamp_request_id(&mut frame, request_id);
        (Some(frame), None)
    };

    let mut content = parsed["content"].as_str().unwrap_or("").to_string();
    if parse_btw_command(&content).is_some() {
        return handle_btw_frame(state, conversation, scope, parsed);
    }
    let original_input = content.clone();
    let ids = match parsed.get("attachments") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(ids))
            if ids.len() <= crate::api_attachments::MAX_MESSAGE_ITEMS =>
        {
            let Some(ids) = ids
                .iter()
                .map(|v| {
                    v.as_str()
                        .filter(|id| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok())
                        .map(str::to_owned)
                })
                .collect::<Option<Vec<_>>>()
            else {
                return reject(
                    &zeroclaw_runtime::i18n::get_required_cli_string(
                        "gateway-attachment-invalid-ids",
                    ),
                    "INVALID_ATTACHMENTS",
                );
            };
            let unique: std::collections::HashSet<_> = ids.iter().collect();
            if unique.len() != ids.len() {
                return reject(
                    &zeroclaw_runtime::i18n::get_required_cli_string("gateway-attachment-repeated"),
                    "INVALID_ATTACHMENTS",
                );
            }
            ids
        }
        Some(_) => {
            return reject(
                &zeroclaw_runtime::i18n::get_required_cli_string("gateway-attachment-count"),
                "INVALID_ATTACHMENTS",
            );
        }
    };
    if !ids.is_empty() && !question_answer_authorized(state, scope) {
        return reject(
            &zeroclaw_runtime::i18n::get_required_cli_string("gateway-attachment-unauthorized"),
            "UNAUTHORIZED_ATTACHMENTS",
        );
    }
    if content.is_empty() && ids.is_empty() {
        return reject("Message content cannot be empty", "EMPTY_CONTENT");
    }

    let durable = match request_id {
        None => false,
        Some(id) => match accept_request(state, conversation, &scope.session_key, id) {
            Ok((RequestReceipt::Recorded, durable)) => durable,
            Ok((RequestReceipt::Duplicate { state: recorded }, _)) => {
                return (
                    Some(serde_json::json!({
                        "type": "ack",
                        "id": id,
                        "status": "duplicate",
                        "state": recorded,
                    })),
                    None,
                );
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_key": scope.session_key,
                            "error": format!("{e}"),
                        })),
                    "WS request could not be recorded"
                );
                return reject(
                    "The request could not be recorded; it was not run",
                    "REQUEST_NOT_RECORDED",
                );
            }
        },
    };
    if !ids.is_empty() {
        let Some((_, agent)) = conversation.key().rsplit_once('\u{1f}') else {
            return reject(
                &zeroclaw_runtime::i18n::get_required_cli_string("gateway-attachment-unavailable"),
                "ATTACHMENT_UNAVAILABLE",
            );
        };
        let payload_scope = crate::api_attachments::Scope {
            subject: scope.auth_subject.clone().unwrap_or_default(),
            session: scope.session_id.clone(),
            agent: agent.to_string(),
        };
        if !crate::api_attachments::scope_authorized(state, &payload_scope) {
            if let Some(id) = request_id {
                set_request_state(state, conversation, &scope.session_key, id, "rejected");
            }
            return reject(
                &zeroclaw_runtime::i18n::get_required_cli_string("gateway-attachment-unauthorized"),
                "UNAUTHORIZED_ATTACHMENTS",
            );
        }
        content =
            match state
                .ws_conversations
                .attachments
                .materialize(&payload_scope, &ids, &content)
            {
                Ok(content) => content,
                Err(_) => {
                    if let Some(id) = request_id {
                        set_request_state(state, conversation, &scope.session_key, id, "rejected");
                    }
                    return reject(
                        &zeroclaw_runtime::i18n::get_required_cli_string(
                            "gateway-attachment-unavailable",
                        ),
                        "ATTACHMENT_UNAVAILABLE",
                    );
                }
            };
        // Resolving/encoding bytes can race a live revoke or agent disable.
        // Consult canonical policy again at the input submission boundary.
        if !crate::api_attachments::scope_authorized(state, &payload_scope) {
            if let Some(id) = request_id {
                set_request_state(state, conversation, &scope.session_key, id, "rejected");
            }
            return reject(
                &zeroclaw_runtime::i18n::get_required_cli_string("gateway-attachment-unauthorized"),
                "UNAUTHORIZED_ATTACHMENTS",
            );
        }
    }
    let ack = |turn: &str| {
        request_id.map(|id| {
            serde_json::json!({
                "type": "ack",
                "id": id,
                "status": "accepted",
                "turn": turn,
                "durable": durable,
            })
        })
    };

    match conversation.submit(content) {
        Submitted::Start(mut claim) => {
            claim.original_input = Some(original_input);
            claim.request_id = request_id.map(str::to_string);
            claim.surface = scope.surface;
            (ack("started"), Some(*claim))
        }
        Submitted::Steered => {
            if let Some(id) = request_id {
                set_request_state(state, conversation, &scope.session_key, id, "steered");
            }
            (ack("steered"), None)
        }
        Submitted::SteeringFull => {
            if let Some(id) = request_id {
                set_request_state(state, conversation, &scope.session_key, id, "rejected");
            }
            reject(
                "Steering queue is full for the running turn",
                "STEERING_QUEUE_FULL",
            )
        }
        Submitted::SteeringClosed => {
            if let Some(id) = request_id {
                set_request_state(state, conversation, &scope.session_key, id, "rejected");
            }
            reject(
                "Running turn is no longer accepting steering messages",
                "STEERING_CLOSED",
            )
        }
    }
}

/// Extract the question body from a leading `/btw` slash command.
///
/// Returns `Some("")` for a bare `"/btw"` so callers reject it as an empty
/// side query rather than forwarding `"/btw"` as a regular user turn.
pub(super) fn parse_btw_command(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    let rest = trimmed.strip_prefix("/btw")?;
    if rest.is_empty() {
        Some("")
    } else if rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

/// Handle a read-only `/btw` side query (`{"type":"btw","content":"..."}` or
/// `{"type":"message","content":"/btw ..."}`).
///
/// Side queries run against a lock-free snapshot of the conversation's `Agent`
/// without acquiring `conversation.agent` or `state.session_queue`, and without
/// mutating conversation history, session storage, request receipts, steering
/// queues, or owner correction authority.
fn handle_btw_frame(
    state: &AppState,
    conversation: &Arc<Conversation<WsSession>>,
    scope: &WsTurnScope,
    parsed: &serde_json::Value,
) -> (Option<serde_json::Value>, Option<TurnClaim>) {
    let request_id = match parsed.get("id") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(id))
            if !id.is_empty()
                && id.len() <= MAX_REQUEST_ID_LEN
                && !id.chars().any(char::is_control) =>
        {
            Some(id.as_str())
        }
        Some(_) => {
            return (
                Some(serde_json::json!({
                    "type": "error",
                    "message": format!(
                        "message id must be a non-empty string of at most {MAX_REQUEST_ID_LEN} characters without control characters"
                    ),
                    "code": "INVALID_REQUEST_ID",
                })),
                None,
            );
        }
    };
    let reject = |message: &str, code: &str| {
        let mut frame = serde_json::json!({ "type": "error", "message": message, "code": code });
        stamp_request_id(&mut frame, request_id);
        (Some(frame), None)
    };

    // Authenticated attachments retain live credential checks on this side
    // route; anonymous legacy chat keeps its existing pairing-off policy.
    if scope.auth_subject.is_some() && !question_answer_authorized(state, scope) {
        let mut frame = device::refusal("BRIDGE_OPERATION_DENIED");
        stamp_request_id(&mut frame, request_id);
        return (Some(frame), None);
    }

    if parsed.get("source").is_some() {
        return reject(
            "/btw side queries do not accept source envelopes",
            "UNSUPPORTED_FIELD",
        );
    }
    let has_attachments = parsed
        .get("attachments")
        .is_some_and(|v| !v.is_null() && !v.as_array().is_some_and(Vec::is_empty));
    if has_attachments {
        return reject(
            "/btw side queries do not accept attachments",
            "INVALID_ATTACHMENTS",
        );
    }

    let raw = parsed["content"]
        .as_str()
        .or_else(|| parsed["question"].as_str())
        .unwrap_or("");
    let question = parse_btw_command(raw).unwrap_or_else(|| raw.trim());
    if question.is_empty() {
        return reject(
            "Side query (/btw) question cannot be empty",
            "EMPTY_CONTENT",
        );
    }
    if question.len() > zeroclaw_runtime::agent::side_query::MAX_SIDE_QUERY_QUESTION_BYTES {
        return reject(
            "Side query (/btw) question exceeds maximum length",
            "CONTENT_TOO_LARGE",
        );
    }

    let Some(side_query) = conversation.side_query.clone() else {
        return reject(
            "Side query (/btw) is unavailable for this conversation",
            "BTW_UNAVAILABLE",
        );
    };
    let Some(permit) = side_query.try_acquire_slot() else {
        return reject(
            "Too many concurrent /btw side queries in flight for this conversation",
            "BTW_BUSY",
        );
    };

    let _task = spawn_ws_btw_query(
        state,
        conversation,
        side_query,
        permit,
        question.to_string(),
        request_id.map(str::to_owned),
    );

    let ack = request_id.map(|id| {
        serde_json::json!({
            "type": "ack",
            "id": id,
            "status": "accepted",
            "turn": "btw",
            "durable": false,
        })
    });
    (ack, None)
}

pub(super) fn spawn_ws_btw_query(
    state: &AppState,
    conversation: &Arc<Conversation<WsSession>>,
    side_query: zeroclaw_runtime::agent::SideQueryHandle,
    permit: tokio::sync::OwnedSemaphorePermit,
    question: String,
    request_id: Option<String>,
) -> tokio::task::JoinHandle<()> {
    let retention = state.ws_conversations.retain_for_side_query(conversation);
    let state = state.clone();
    let conversation = Arc::clone(conversation);
    let (reset_revision, cancel) = conversation.side_query_generation();
    let observed_turn_id = conversation.active_turn_id();
    let (_, agent_alias) = conversation
        .key()
        .rsplit_once('\u{1f}')
        .unwrap_or((conversation.key(), ""));
    let cost_tracking_context = state.cost_tracker.as_ref().map(|tracker| {
        let config = state.config.read();
        let pricing = zeroclaw_runtime::agent::cost::build_model_provider_pricing(&config);
        zeroclaw_runtime::agent::cost::ToolLoopCostTrackingContext::new(
            tracker.clone(),
            Arc::new(pricing),
        )
        .with_agent_alias(agent_alias)
    });
    let turn_usage = state.cost_tracker.as_ref().map(|_| {
        Arc::new(parking_lot::Mutex::new(
            zeroclaw_runtime::agent::cost::TurnUsage::default(),
        ))
    });
    zeroclaw_spawn::spawn!(async move {
        let _retention = retention;
        let _permit = permit;
        let query_result = zeroclaw_runtime::agent::cost::TOOL_LOOP_TURN_USAGE
            .scope(
                turn_usage,
                zeroclaw_runtime::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                    cost_tracking_context,
                    side_query.ask(&question, Some(&cancel)),
                ),
            )
            .await;
        if cancel.is_cancelled() {
            return;
        }
        match query_result {
            Ok(resp) => {
                let leak_detection = state.config.read().security.leak_detection.clone();
                let safe_question = redact_frame_text(&resp.question, &leak_detection);
                let sanitized = zeroclaw_runtime::security::outbound::sanitize_outbound_response(
                    &resp.answer,
                    &resp.known_tool_names,
                    &leak_detection,
                    zeroclaw_runtime::security::outbound::OutboundContentFormat::Markdown,
                );
                let mut frame = serde_json::json!({
                    "type": "btw_response",
                    "question": safe_question,
                    "content": sanitized,
                    "full_response": sanitized,
                    "model": resp.model,
                    "provider": resp.provider_name,
                    "revision": resp.revision,
                });
                if let Some(turn_id) = observed_turn_id.or(resp.active_turn_id) {
                    frame["turn_id"] = serde_json::Value::String(turn_id);
                }
                stamp_request_id(&mut frame, request_id.as_deref());
                conversation.publish_side_query(&frame, reset_revision);
            }
            Err(err) => {
                let leak_detection = state.config.read().security.leak_detection.clone();
                let safe_question = redact_frame_text(&question, &leak_detection);
                let scrubbed = zeroclaw_providers::sanitize_api_error(&err.to_string());
                let safe_message = redact_frame_text(&scrubbed, &leak_detection);
                let mut frame = serde_json::json!({
                    "type": "btw_error",
                    "question": safe_question,
                    "message": safe_message,
                    "code": "BTW_FAILED",
                });
                if let Some(turn_id) = observed_turn_id {
                    frame["turn_id"] = serde_json::Value::String(turn_id);
                }
                stamp_request_id(&mut frame, request_id.as_deref());
                conversation.publish_side_query(&frame, reset_revision);
            }
        }
    })
}

/// Record a client request as accepted: in the session store when it keeps
/// receipts, otherwise in the hub's bounded reconnect memory. The flag says whether
/// the record survives a restart.
fn accept_request(
    state: &AppState,
    _conversation: &Conversation<WsSession>,
    session_key: &str,
    request_id: &str,
) -> std::io::Result<(RequestReceipt, bool)> {
    if let Some(backend) = &state.session_backend
        && let Some(receipt) = backend.record_request(session_key, request_id, "accepted")?
    {
        return Ok((receipt, true));
    }
    Ok((
        state
            .ws_conversations
            .record_request(session_key, request_id, "accepted")?,
        false,
    ))
}

/// Move a recorded request to `request_state`, wherever it was recorded.
fn set_request_state(
    state: &AppState,
    _conversation: &Conversation<WsSession>,
    session_key: &str,
    request_id: &str,
    request_state: &str,
) {
    state
        .ws_conversations
        .set_request_state(session_key, request_id, request_state);
    if let Some(backend) = &state.session_backend
        && let Err(e) = backend.set_request_state(session_key, request_id, request_state)
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "session_key": session_key,
                    "request_id": request_id,
                    "error": format!("{e}"),
                })),
            "WS request state could not be recorded"
        );
    }
}

/// Add the client's request id to a frame about that request.
fn stamp_request_id(frame: &mut serde_json::Value, request_id: Option<&str>) {
    if let (Some(id), Some(fields)) = (request_id, frame.as_object_mut()) {
        fields.insert("id".into(), id.into());
    }
}

fn start_ws_turns(
    state: &AppState,
    conversation: &Arc<Conversation<WsSession>>,
    scope: &WsTurnScope,
    claim: TurnClaim,
) {
    let turns = run_ws_turns(
        state.clone(),
        Arc::clone(conversation),
        scope.clone(),
        claim,
    );
    zeroclaw_spawn::spawn!(turns);
}

/// Run a claimed turn to completion. Only legacy admissions may turn unread
/// steering into a follow-up; S2 steering remains bound to its admitted turn.
async fn run_ws_turns(
    state: AppState,
    conversation: Arc<Conversation<WsSession>>,
    mut scope: WsTurnScope,
    claim: TurnClaim,
) {
    let mut next = Some(claim);
    let lifecycle = state.ws_conversations.session_lifecycle(&scope.session_key);
    // Each idle socket submission starts its own invocation with that
    // socket's scope. Joined late steering does not inherit its authorship.
    let mut initial_claim = true;
    while let Some(TurnClaim {
        input,
        original_input,
        request_id,
        intake,
        generation,
        reset_revision,
        turn_id,
        cancel,
        mut steering,
        surface,
    }) = next.take()
    {
        // Snapshot the admitted contract before any awaits or settlement.
        // Check the scoped receipt AND turn, never a caller-chosen id prefix.
        // A terminal S2 turn cannot promote unread steering to new work, even
        // on provider error, budget exhaustion, or failed admission rechecks.
        let scoped_steering = request_id.as_deref().is_some_and(|request| {
            state
                .ws_conversations
                .turn_control
                .owns_request_for_turn(request, &turn_id)
        });
        scope.surface = surface;
        let mut intake_started = intake.is_none();
        let mut allow_resume = false;
        let (late, outcome) = match state.session_queue.acquire(&scope.session_key).await {
            Ok(_session_guard) => {
                let mut session = conversation.agent.lock().await;
                let alias = conversation
                    .key()
                    .rsplit_once('\u{1f}')
                    .map(|(_, alias)| alias)
                    .unwrap_or("");
                let permitted = device::permitted(&state, &scope, alias, "chat");
                let admitted = if reset_revision != conversation.reset_revision() {
                    Ok(false)
                } else if !permitted {
                    let mut denied = device::refusal("BRIDGE_OPERATION_DENIED");
                    stamp_request_id(&mut denied, request_id.as_deref());
                    turn_control::publish(
                        &state,
                        &conversation,
                        request_id.as_deref(),
                        &turn_id,
                        &denied,
                    );
                    Ok(false)
                } else {
                    intake::begin(&state, &conversation, &scope, intake.as_ref())
                };
                allow_resume = permitted && admitted.is_ok();
                if !admitted.is_ok_and(|started| started) {
                    conversation.finish_turn(generation);
                    // Legacy clients may already have received a steering
                    // ACK while this durable claim awaited admission.
                    let late = std::iter::from_fn(|| steering.try_recv().ok()).collect();
                    (late, "error")
                } else {
                    intake_started = true;
                    process_chat_message(
                        &state,
                        &conversation,
                        &mut session,
                        &scope,
                        initial_claim && intake.is_none() && scope.bridge.is_none(),
                        original_input.as_deref(),
                        &input,
                        request_id.as_deref(),
                        &turn_id,
                        generation,
                        cancel,
                        &mut steering,
                    )
                    .await
                }
            }
            Err(e) => {
                conversation.finish_turn(generation);
                let mut frame = serde_json::json!({
                    "type": "error",
                    "message": e.to_string(),
                    "code": session_queue_ws_error_code(&e)
                });
                stamp_request_id(&mut frame, request_id.as_deref());
                turn_control::publish(
                    &state,
                    &conversation,
                    request_id.as_deref(),
                    &turn_id,
                    &frame,
                );
                (Vec::new(), "error")
            }
        };
        // Execution releases its queue guard before taking this lock: DELETE
        // takes the lifecycle guard first, then waits for execution to settle.
        let _lifecycle = lifecycle.lock().await;
        if reset_revision != conversation.reset_revision() {
            if let Some(claim) = &intake {
                intake::release(&state, claim);
            }
            break;
        }
        if let Some(claim) = &intake {
            if intake_started {
                intake::finish(&state, &conversation, claim, outcome);
            } else {
                intake::release(&state, claim);
                conversation.publish(&serde_json::json!({"type":"error","id":request_id,"code":"SOURCE_NOT_STARTED",
                    "message":zeroclaw_runtime::i18n::get_required_cli_string("gateway-intake-unavailable")}));
            }
        } else if let Some(id) = &request_id {
            set_request_state(&state, &conversation, &scope.session_key, id, outcome);
        }
        turn_control::finish(
            &state,
            &conversation,
            request_id.as_deref(),
            &turn_id,
            outcome,
        );
        if !scoped_steering
            && !late.is_empty()
            && let Submitted::Start(mut claim) = conversation.submit(late.join("\n\n"))
        {
            claim.surface = scope.surface;
            next = Some(*claim);
        }
        if next.is_none()
            && allow_resume
            && let Some((next_scope, claim)) = intake::resume_registered(&state, &conversation)
        {
            scope = next_scope;
            next = Some(claim);
        }
        initial_claim = false;
    }
    state.ws_conversations.release_if_unused(&conversation);
}

/// Record the owning agent and optional name for a session on connect, and
/// return the name to report to the client (the requested one, else the
/// stored one).
///
/// The alias is written first because it upserts the metadata row; the name
/// write only updates an existing row, so a new session's name would
/// otherwise be dropped.
fn stamp_session(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    session_key: &str,
    agent_alias: &str,
    requested_name: Option<&str>,
) -> Option<String> {
    let _ = backend.set_session_agent_alias(session_key, agent_alias);
    if let Some(name) = requested_name.filter(|name| !name.is_empty()) {
        let _ = backend.set_session_name(session_key, name);
        return Some(name.to_string());
    }
    backend.get_session_name(session_key).unwrap_or(None)
}

fn resolve_session_cwd(
    requested_cwd: Option<&str>,
    default_workspace: &Path,
) -> anyhow::Result<PathBuf> {
    let cwd = requested_cwd
        .map(PathBuf::from)
        .unwrap_or_else(|| default_workspace.to_path_buf());
    let resolved = std::fs::canonicalize(&cwd).map_err(|e| {
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
    })?;
    let workspace = std::fs::canonicalize(default_workspace).map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "workspace": default_workspace.display().to_string(),
                    "error": format!("{}", e),
                })),
            "ws session workspace rejected"
        );
        anyhow::Error::msg(format!(
            "{}: {e}",
            zeroclaw_runtime::i18n::get_required_cli_string("gateway-cwd-workspace-unavailable")
        ))
    })?;
    anyhow::ensure!(
        resolved.is_dir() && workspace.is_dir(),
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string("gateway-cwd-not-directory")
    );
    // Compare canonical path components, not string prefixes: a remote cwd
    // becomes the Agent policy root and must never widen its configured scope.
    anyhow::ensure!(
        resolved.starts_with(&workspace),
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string("gateway-cwd-outside-workspace")
    );
    Ok(resolved)
}

fn resolve_ws_session_cwd(
    requested_cwd: Option<&str>,
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> anyhow::Result<PathBuf> {
    let agent_workspace = config.agent_workspace_dir(agent_alias);
    if requested_cwd.is_none() {
        std::fs::create_dir_all(&agent_workspace).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "agent": agent_alias,
                        "cwd": agent_workspace.display().to_string(),
                        "error": format!("{}", e),
                    })),
                "ws agent workspace cwd rejected"
            );
            anyhow::Error::msg(format!(
                "cwd is not a usable directory ({}): {e}",
                agent_workspace.display()
            ))
        })?;
    }
    resolve_session_cwd(requested_cwd, &agent_workspace)
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
    ingress: Option<&zeroclaw_api::review::UserMessageIngress>,
) {
    // if the user deleted the session between the turn starting and
    // the post-turn persistence, don't resurrect it. The `aborted` / `done`
    // / `error` frames are still sent to the client; we just refuse to
    // re-create the row that `DELETE /api/sessions/{id}` just wiped.
    if !backend.session_exists(session_key) {
        return;
    }
    let mut initial_user = true;
    for message in messages {
        let zeroclaw_providers::ConversationMessage::Chat(message) = message else {
            continue;
        };
        if message.role == "system" {
            continue;
        }
        if message.role == "user"
            && std::mem::take(&mut initial_user)
            && let Some(ingress) = ingress
        {
            let _ = backend.append_with_ingress(session_key, message, ingress);
        } else {
            let _ = backend.append(session_key, message);
        }
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

fn history_trimmed_ws_frame(
    dropped_messages: usize,
    kept_turns: usize,
    reason: &str,
    leak_detection: &zeroclaw_config::schema::LeakDetectionConfig,
) -> serde_json::Value {
    let reason = redact_frame_text(reason, leak_detection);
    serde_json::json!({
        "type": "history_trimmed",
        "dropped_messages": dropped_messages,
        "kept_turns": kept_turns,
        "reason": reason,
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

/// Only events addressed to this session reach a chat socket. Events
/// without a `session_id` (cron results, observability) stay on the SSE
/// stream; proactive messages reach people through the bridge outbox.
fn event_matches_session(event: &serde_json::Value, session_id: &str) -> bool {
    event.get("session_id").and_then(|value| value.as_str()) == Some(session_id)
}

fn is_observability_telemetry(event: &serde_json::Value) -> bool {
    event.get("source").and_then(serde_json::Value::as_str) == Some("observability")
}

/// Provider, model, and temperature for turn-end memory consolidation,
/// built from the agent's live `<family>.<alias>` reference and model as
/// they stand after the turn. A mid-session model switch changes the agent's
/// provider and model together, so consolidation follows that pair instead of
/// the gateway's boot default: the turn's content only goes to the provider
/// that already saw it. An empty model falls back to the entry's configured
/// model. `None` when the reference no longer resolves (for example after a
/// config reload removed the entry); consolidation is then skipped, never
/// routed to another provider.
fn ws_consolidation_model(
    config: &zeroclaw_config::schema::Config,
    provider_ref: &str,
    model: &str,
) -> Option<(
    Box<dyn zeroclaw_api::model_provider::ModelProvider>,
    String,
    Option<f64>,
)> {
    let (family, alias) = provider_ref.split_once('.')?;
    let temperature = config.providers.models.find(family, alias)?.temperature;
    let (provider, _, model) = zeroclaw_runtime::agent::agent::build_session_model_provider(
        config,
        provider_ref,
        Some(model),
    )
    .ok()?;
    Some((provider, model, temperature))
}

/// Run one chat turn through the conversation's agent and publish its
/// frames to every subscriber. Uses [`Agent::turn_streamed`] so that
/// intermediate text chunks, tool calls, and tool results reach the clients
/// in real time. Returns steering messages that arrived after the agent
/// stopped reading them, which the caller runs as the next turn, and the
/// turn's outcome (`done`, `aborted` or `error`). Its terminal frame carries
/// the client's `request_id` when it sent one.
#[allow(clippy::too_many_arguments)]
async fn process_chat_message(
    state: &AppState,
    conversation: &Conversation<WsSession>,
    session: &mut WsSession,
    scope: &WsTurnScope,
    initial_operator_input: bool,
    original_input: Option<&str>,
    content: &str,
    request_id: Option<&str>,
    turn_id: &str,
    generation: u64,
    cancel_token: tokio_util::sync::CancellationToken,
    steering_rx: &mut tokio::sync::mpsc::Receiver<String>,
) -> (Vec<String>, &'static str) {
    use zeroclaw_runtime::agent::TurnEvent;

    // A paired transport authenticates the client, not the person speaking.
    // Voice has no verified-speaker contract and therefore no owner learning.
    let initial_operator_input = initial_operator_input
        && scope.surface != Some(ChatSurface::Voice)
        && scope.bridge.is_none();
    let WsSession { agent, ws_memory } = session;
    let session_key = scope.session_key.as_str();
    // Resolve canonical paired-device membership when storing this input.
    // Bridge/anonymous sockets and unbound steering receive no owner source.
    let owner_ingress = || {
        (initial_operator_input
            && scope
                .auth_subject
                .as_ref()
                .is_some_and(|subject| state.pairing.tokens().contains(subject)))
        .then_some(original_input)
        .flatten()
        .map(|text| zeroclaw_api::review::UserMessageIngress {
            source: zeroclaw_api::review::UserMessageSource::Operator,
            text: text.to_string(),
        })
    };

    let (turn_alias, turn_provider, turn_model) = agent.attribution_fields();
    let provider_label = turn_provider.clone();
    let cost_tracking_context = state.cost_tracker.as_ref().map(|tracker| {
        let config = state.config.read();
        let pricing = zeroclaw_runtime::agent::cost::build_model_provider_pricing(&config);
        zeroclaw_runtime::agent::cost::ToolLoopCostTrackingContext::new(
            tracker.clone(),
            Arc::new(pricing),
        )
        .with_agent_alias(&turn_alias)
    });
    let turn_usage = state.cost_tracker.as_ref().map(|_| {
        Arc::new(parking_lot::Mutex::new(
            zeroclaw_runtime::agent::cost::TurnUsage::default(),
        ))
    });

    // Resolve context budget for this agent. Wire field is named
    // `max_context_tokens` and must track the runtime-profile budget
    // (same source Zerocode's context meter uses), not the provider
    // model-window helper which falls back to 32_000 when unset.
    let max_context_tokens = {
        let cfg = state.config.read();
        cfg.effective_max_context_tokens(&turn_alias) as u64
    };

    // Broadcast agent_start event
    let _ = state.event_tx.send(serde_json::json!({
        "type": "agent_start",
        "model_provider": provider_label,
        "model": turn_model,
    }));

    // Set session state to running
    if let Some(ref backend) = state.session_backend {
        let _ = backend.set_session_state(session_key, "running", Some(turn_id));
    }

    // ── Cancellation token lifecycle ─────────────────────────────
    // Register the turn's token so the abort endpoint can cancel it.
    // Remove it after the turn completes regardless of outcome
    // (normal, error, or cancelled).
    {
        state
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .insert(session_key.to_string(), cancel_token.clone());
    }

    // Outbound sanitization, shared with the channels: chunks go through a
    // stream redactor that holds back any tail that could still become a
    // credential or a protocol envelope, and the done frame's full response
    // gets the channels' final pass.
    let known_tool_names =
        zeroclaw_runtime::security::outbound::known_tool_names(agent.tool_names());
    let leak_detection = state.config.read().security.leak_detection.clone();
    let mut chunk_redactor = zeroclaw_runtime::security::outbound::OutboundStreamRedactor::new(
        known_tool_names.clone(),
        &leak_detection,
        zeroclaw_runtime::security::outbound::OutboundContentFormat::Markdown,
    );

    // Channel for streaming turn events from the agent.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);

    let content_owned = content.to_string();
    let session_key_owned = session_key.to_string();
    let correction_pairing = Arc::clone(&state.pairing);
    let correction_subject = scope.auth_subject.clone();
    let correction_context =
        original_input.map(|text| zeroclaw_api::review::OwnerCorrectionContext {
            agent_alias: turn_alias.clone(),
            session_key: session_key_owned.clone(),
            ingress: zeroclaw_api::review::UserMessageIngress {
                source: zeroclaw_api::review::UserMessageSource::Operator,
                text: text.to_string(),
            },
        });
    let correction_invalidated = conversation.correction_invalidation(generation);
    let correction_resolver: zeroclaw_api::review::OwnerCorrectionResolver = Arc::new(move || {
        (initial_operator_input
            && correction_invalidated
                .as_ref()
                .is_some_and(|invalidated| !invalidated.load(std::sync::atomic::Ordering::Acquire))
            && correction_subject
                .as_ref()
                .is_some_and(|subject| correction_pairing.tokens().contains(subject)))
        .then(|| correction_context.clone())
        .flatten()
    });
    let turn_fut = async {
        use ::zeroclaw_log::Instrument as _;
        let span = ::zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            session_key = %session_key_owned,
            agent_alias = %turn_alias,
            model_provider = %turn_provider,
            model = %turn_model,
            channel = WS_CHANNEL_KEY,
        );
        zeroclaw_runtime::agent::loop_::scope_session_key(
            Some(session_key_owned.clone()),
            zeroclaw_runtime::agent::cost::TOOL_LOOP_TURN_USAGE.scope(
                turn_usage.clone(),
                zeroclaw_runtime::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                    cost_tracking_context.clone(),
                    surface::CURRENT.scope(
                        scope.surface,
                        agent
                            .turn_streamed_with_steering_state(
                                &content_owned,
                                event_tx,
                                Some(cancel_token.clone()),
                                Some(&mut *steering_rx),
                            )
                            .instrument(span),
                    ),
                ),
            ),
        )
        .await
    };

    let turn_fut: std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send + '_>> = Box::pin(
        zeroclaw_api::review::OWNER_CORRECTION_CONTEXT.scope(correction_resolver, turn_fut),
    );

    // Drive both futures concurrently: the agent turn produces events
    // and we relay them over WebSocket. Track streamed chunks so we
    // can reconstruct partial content on cancellation.
    let mut accumulated_text = String::new();

    // Aggregate token usage across all LLM calls in this turn.
    // The agent emits TurnEvent::Usage once per LLM call when the provider
    // surfaces usage; we sum to produce a single done-frame total.
    let mut total_input_tokens: Option<u64> = None;
    let mut total_output_tokens: Option<u64> = None;

    // Track the most recent absolute provider-reported prompt size
    // (replaces on each TurnEvent::Usage; not accumulated).
    // Used for accurate context-bar rendering on the client.
    let mut last_input_tokens: Option<u64> = None;

    let forward_fut = async {
        let mut cancel_drained = false;
        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled(), if !cancel_drained => {
                    conversation.drain_approvals();
                    cancel_drained = true;
                    // Fall through; the agent loop will now wake from the
                    // approval await, see the cancel token, and propagate
                    // a ToolLoopCancelled error which closes event_rx and
                    // breaks this loop on the `event_rx.recv()` arm below.
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
                            if let Some(it) = input_tokens {
                                total_input_tokens = Some(total_input_tokens.unwrap_or(0) + it);
                                last_input_tokens = Some(it);
                            }
                            if let Some(ot) = output_tokens {
                                total_output_tokens = Some(total_output_tokens.unwrap_or(0) + ot);
                            }
                            continue;
                        }
                        TurnEvent::Chunk { ref delta } => {
                            accumulated_text.push_str(delta);
                            let Some(visible) = chunk_redactor.push(delta) else {
                                continue;
                            };
                            serde_json::json!({ "type": "chunk", "content": visible })
                        }
                        other => turn_event_ws_frame(other, &leak_detection),
                    };
                    turn_control::publish(state, conversation, request_id, turn_id, &ws_msg);
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

    // The agent no longer reads steering. Free the turn slot first, so a
    // message sent from here on starts a new turn instead of steering this
    // one, then collect what was queued but never read.
    conversation.finish_turn(generation);
    steering_rx.close();
    let mut late = Vec::new();
    while let Ok(message) = steering_rx.try_recv() {
        late.push(message);
    }

    // Check if this turn was cancelled. `turn_streamed` propagates
    // `ToolLoopCancelled` through anyhow, so we detect it here.
    let was_cancelled = match &result {
        Err(e) => zeroclaw_runtime::agent::loop_::is_tool_loop_cancelled(&e.error),
        Ok(_) => false,
    };

    if was_cancelled {
        if let Some(ref backend) = state.session_backend {
            let still_exists = backend.session_exists(session_key);
            if still_exists {
                match &result {
                    Err(error) if !error.new_messages.is_empty() => {
                        persist_conversation_messages(
                            backend.as_ref(),
                            session_key,
                            &error.new_messages,
                            owner_ingress().as_ref(),
                        );
                        if !has_assistant_chat_message(&error.new_messages) {
                            let marker = zeroclaw_runtime::i18n::get_required_cli_string(
                                "turn-interrupted-by-user",
                            );
                            let truncated = if accumulated_text.is_empty() {
                                marker
                            } else {
                                format!("{accumulated_text}\n\n{marker}")
                            };
                            let assistant_msg =
                                zeroclaw_providers::ChatMessage::assistant(&truncated);
                            // Re-check before the raw append — the user can
                            // delete the session between the outer check and
                            // here; `persist_conversation_messages` already
                            // re-checks internally.
                            if backend.session_exists(session_key) {
                                let _ = backend.append(session_key, &assistant_msg);
                            }
                        }
                    }
                    _ => {
                        let marker = zeroclaw_runtime::i18n::get_required_cli_string(
                            "turn-interrupted-by-user",
                        );
                        let truncated = if accumulated_text.is_empty() {
                            marker
                        } else {
                            format!("{accumulated_text}\n\n{marker}")
                        };
                        let assistant_msg = zeroclaw_providers::ChatMessage::assistant(&truncated);
                        if backend.session_exists(session_key) {
                            let _ = backend.append(session_key, &assistant_msg);
                        }
                    }
                }
            }
        }

        // Inform the client the turn was aborted
        let mut aborted = serde_json::json!({ "type": "aborted" });
        stamp_request_id(&mut aborted, request_id);
        turn_control::publish(state, conversation, request_id, turn_id, &aborted);

        if let Some(ref backend) = state.session_backend
            && backend.session_exists(session_key)
        {
            let _ = backend.set_session_state(session_key, "idle", None);
        }

        // Broadcast agent_end event
        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_end",
            "model_provider": provider_label,
            "model": turn_model,
        }));

        // Trace the cancelled turn so the doctor / replay tool sees it
        // alongside successful turns.follow-through.
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "model_provider": provider_label,
                    "model": turn_model,
                    "session_key": session_key,
                    "reason": "interrupted by user",
                    "cancelled": true,
                    "trace_id": turn_id,
                })),
            "gateway_ws_turn"
        );

        // A cancel stops the conversation's work, queued steering included.
        return (Vec::new(), "aborted");
    }

    let settled = if result.is_ok() { "done" } else { "error" };
    match result {
        Ok(outcome) => {
            if let Some(ref backend) = state.session_backend {
                persist_conversation_messages(
                    backend.as_ref(),
                    session_key,
                    &outcome.new_messages,
                    owner_ingress().as_ref(),
                );
            }

            // Fire-and-forget curated-memory consolidation (sqlite Memory).
            // Companion capture is a separate seam and already ran above.
            if state.auto_save {
                if let Some(mem) = ws_memory.clone() {
                    // The agent's provider/model after the turn, not the
                    // gateway-wide boot default (upstream #10637).
                    let (_, live_provider_ref, live_model) = agent.attribution_fields();
                    let live_config = Arc::clone(&state.config);
                    let memory_config = state.config.read().memory.clone();
                    let user_msg = content.to_string();
                    let assistant_resp = outcome.response.clone();
                    zeroclaw_spawn::spawn!(async move {
                        let config = live_config.read().clone();
                        let Some((model_provider, model, temperature)) =
                            ws_consolidation_model(&config, &live_provider_ref, &live_model)
                        else {
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({
                                    "model_provider": &live_provider_ref,
                                })),
                                "WS memory consolidation skipped: provider no longer resolves"
                            );
                            return;
                        };
                        if let Err(e) = zeroclaw_memory::consolidation::consolidate_turn(
                            model_provider.as_ref(),
                            &model,
                            temperature,
                            mem.as_ref(),
                            &memory_config,
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
                } else {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "WS memory consolidation skipped"
                    );
                }
            }

            let total_tokens = match (total_input_tokens, total_output_tokens) {
                (Some(i), Some(o)) => Some(i.saturating_add(o)),
                (Some(i), None) => Some(i),
                (None, Some(o)) => Some(o),
                (None, None) => None,
            };
            let cost_usd = turn_usage
                .as_ref()
                .map(|usage| *usage.lock())
                .filter(|usage| usage.input_tokens > 0 || usage.output_tokens > 0)
                .map(|usage| usage.cost_usd);

            let full_response = zeroclaw_runtime::security::outbound::sanitize_outbound_response(
                &outcome.response,
                &known_tool_names,
                &leak_detection,
                zeroclaw_runtime::security::outbound::OutboundContentFormat::Markdown,
            );
            let done = serde_json::json!({
                "type": "done",
                "full_response": full_response,
                "input_tokens": total_input_tokens,
                "output_tokens": total_output_tokens,
                "tokens_used": total_tokens,
                "cost_usd": cost_usd,
                "model": outcome.model,
                "provider": provider_label,
                "provider_ref": outcome.provider_name,
                "max_context_tokens": max_context_tokens,
                "last_input_tokens": last_input_tokens,
            });
            let mut done = done;
            stamp_request_id(&mut done, request_id);
            turn_control::publish(state, conversation, request_id, turn_id, &done);

            // Set session state to idle
            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "idle", None);
            }

            // Broadcast agent_end event
            let _ = state.event_tx.send(serde_json::json!({
                "type": "agent_end",
                "model_provider": provider_label,
                "model": outcome.model,
                "provider_ref": outcome.provider_name,
            }));

            // Append a runtime-trace.jsonl record so a `zeroclaw doctor`
            // sweep sees gateway WS turns alongside channel and CLI turns.
            // Closes the gateway-side trace gap from
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "model_provider": provider_label,
                        "model": outcome.model,
                        "provider_ref": outcome.provider_name,
                        "session_key": session_key,
                        "input_tokens": total_input_tokens,
                        "output_tokens": total_output_tokens,
                        "tokens_used": total_tokens,
                        "cost_usd": cost_usd,
                        "last_input_tokens": last_input_tokens,
                        "trace_id": turn_id,
                    })),
                "gateway_ws_turn"
            );
        }
        Err(e) => {
            if let Some(ref backend) = state.session_backend
                && !e.new_messages.is_empty()
            {
                persist_conversation_messages(
                    backend.as_ref(),
                    session_key,
                    &e.new_messages,
                    owner_ingress().as_ref(),
                );
            }

            // Set session state to error
            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "error", Some(turn_id));
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
            let mut err = err;
            stamp_request_id(&mut err, request_id);
            turn_control::publish(state, conversation, request_id, turn_id, &err);

            // Broadcast error event
            let _ = state.event_tx.send(serde_json::json!({
                "type": "error",
                "component": "ws_chat",
                "message": sanitized,
            }));

            // Trace the failed turn so the doctor / replay tool sees the
            // failure mode and the turn_id can be cross-referenced with
            // costs.jsonl.follow-through.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "model_provider": provider_label,
                        "model": turn_model,
                        "session_key": session_key,
                        "error": sanitized,
                        "error_code": error_code,
                        "trace_id": turn_id,
                    })),
                "gateway_ws_turn"
            );
        }
    }
    (late, settled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    /// Consolidation follows the agent's live provider reference, with that
    /// entry's model and temperature, not the install-wide default.
    #[test]
    fn ws_consolidation_model_follows_the_live_provider_reference() {
        use zeroclaw_api::attribution::Attributable as _;
        use zeroclaw_config::schema::{
            ModelProviderConfig, OllamaModelProviderConfig, OpenAIModelProviderConfig,
        };
        let mut config = zeroclaw_config::schema::Config::default();
        config.providers.models.openai.insert(
            "install".to_string(),
            OpenAIModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("install-model".to_string()),
                    temperature: Some(0.9),
                    ..Default::default()
                },
            },
        );
        config.providers.models.ollama.insert(
            "local".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("local-model".to_string()),
                    temperature: Some(0.3),
                    ..Default::default()
                },
            },
        );

        let (provider, model, temperature) =
            ws_consolidation_model(&config, "ollama.local", "llama3").unwrap();
        assert_eq!(provider.alias(), "local");
        assert_eq!(model, "llama3");
        assert_eq!(temperature, Some(0.3));

        let (provider, model, temperature) =
            ws_consolidation_model(&config, "ollama.local", "").unwrap();
        assert_eq!(provider.alias(), "local");
        assert_eq!(model, "local-model");
        assert_eq!(temperature, Some(0.3));

        // A reference that no longer resolves is skipped, not rerouted.
        assert!(ws_consolidation_model(&config, "ollama.removed", "llama3").is_none());
        assert!(ws_consolidation_model(&config, "not-dotted", "llama3").is_none());
    }

    #[test]
    fn consolidation_does_not_use_the_gateway_boot_provider() {
        let src = process_chat_message_src();
        let consolidation = src.find("consolidate_turn").expect("consolidation call");
        let block = &src[src[..consolidation].rfind("if state.auto_save").unwrap()..consolidation];
        assert!(!block.contains("state.model_provider"), "{block}");
        assert!(block.contains("ws_consolidation_model"), "{block}");
    }

    fn process_chat_message_src() -> &'static str {
        let src = include_str!("ws.rs");
        let start = src
            .find("async fn process_chat_message")
            .expect("process_chat_message");
        let rest = &src[start..];
        let end = rest
            .find("\n#[cfg(test)]")
            .expect("test module follows process_chat_message");
        &rest[..end]
    }

    #[test]
    fn settled_turns_do_not_write_placeholder_capture_receipts() {
        assert!(!process_chat_message_src().contains("persist_companion_capture"));
    }

    #[test]
    fn ws_turn_has_a_single_channel_identity() {
        // Regression: `Agent.channel_name` was set to "ws" to match the
        // back-channel registration key while the turn span still recorded
        // `channel = "wss"`, so one turn was attributed to two channel names.
        // All three uses now derive from WS_CHANNEL_KEY; this pins the value
        // to the historical ingress name so observability stays stable and
        // interactive-tool lookups still resolve.
        assert_eq!(
            WS_CHANNEL_KEY, "wss",
            "WS ingress identity must stay `wss` — it is the name already used by \
             the turn span and SSE `channel` field; changing it splits attribution"
        );
    }

    #[tokio::test]
    async fn ws_back_channel_registers_under_the_ingress_identity() {
        // The interactive tools (`ask_user`, `poll`, `escalate_to_human`) look
        // the channel up by the agent's channel name. If the registration key
        // and WS_CHANNEL_KEY ever diverge, that lookup misses and the tools
        // silently fall back to an arbitrary seeded channel — the original bug.
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let pending = crate::ws_approval::new_pending_approvals();
        let approval_channel = Arc::new(WsApprovalChannel::new(
            tx,
            pending,
            Duration::from_secs(WS_APPROVAL_TIMEOUT_SECS),
        ));

        let handle: zeroclaw_runtime::tools::PerToolChannelHandle =
            Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()));
        handle.write().insert(
            WS_CHANNEL_KEY.to_string(),
            approval_channel as Arc<dyn zeroclaw_api::channel::Channel>,
        );

        // Interactive tools resolve the back-channel by the agent's channel
        // name; this is the lookup `ask_user` / `poll` / `escalate_to_human`
        // perform against their shared channel map.
        let resolved = handle.read().get(WS_CHANNEL_KEY).cloned();
        assert!(
            resolved.is_some(),
            "back-channel must be resolvable by the same key the agent reports \
             as its channel name ({WS_CHANNEL_KEY})"
        );
        assert!(
            !resolved.unwrap().supports_outbound_send(),
            "WS approval channel must declare that `send` does not deliver, so \
             poll/escalate_to_human fail honestly instead of reporting false success"
        );
    }

    #[test]
    fn restore_trim_uses_live_history_trimmed_frame_shape() {
        let frame = history_trimmed_ws_frame(
            12,
            3,
            "message limit",
            &zeroclaw_config::schema::LeakDetectionConfig::default(),
        );

        assert_eq!(
            frame,
            serde_json::json!({
                "type": "history_trimmed",
                "dropped_messages": 12,
                "kept_turns": 3,
                "reason": "message limit",
            })
        );
    }

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
    fn chat_ws_unaffected_by_nodes_v2_subprotocol_requirement() {
        // `/ws/chat` still upgrades when Sec-WebSocket-Protocol is absent or
        // carries only the nodes token. The nodes v2 handler is fail-closed;
        // this path is not.
        assert!(!client_offers_chat_protocol(&HeaderMap::new()));
        let mut nodes_only = HeaderMap::new();
        nodes_only.insert(
            "sec-websocket-protocol",
            "zeroclaw.nodes.v2".parse().unwrap(),
        );
        assert!(!client_offers_chat_protocol(&nodes_only));
        let mut chat = HeaderMap::new();
        chat.insert("sec-websocket-protocol", "zeroclaw.v1".parse().unwrap());
        assert!(client_offers_chat_protocol(&chat));
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
        // No session_id → dropped.
        let nameless_observability = serde_json::json!({
            "type": "agent_start",
            "source": "observability",
            "model": "gpt-4o"
        });
        // No session_id: cron results are not broadcast to chat sockets.
        let cron = serde_json::json!({
            "type": "cron_result",
            "output": "global notification"
        });

        assert!(event_matches_session(&target_event, "operator-1"));
        assert!(!event_matches_session(&other_event, "operator-1"));
        assert!(!event_matches_session(
            &nameless_observability,
            "operator-1"
        ));
        assert!(!event_matches_session(&cron, "operator-1"));
    }

    #[test]
    fn event_matches_session_defaults_drops_unwhitelisted_no_session_frames() {
        // The pre-contract was `None => true`, which silently leaked
        // every BroadcastObserver telemetry frame (including `error`) into
        // every chat WebSocket. The fix flips the default; verify each
        // observed-in-the-wild leak shape is now blocked.
        for ty in [
            "agent_start",
            "agent_end",
            "llm_request",
            "tool_call",
            "tool_call_start",
            "error",
        ] {
            let frame = serde_json::json!({
                "type": ty,
                "source": "observability",
                "timestamp": "2026-06-04T00:00:00Z",
            });
            assert!(
                !event_matches_session(&frame, "operator-1"),
                "{ty} observability frame must be dropped from chat WS"
            );
        }
    }

    #[tokio::test]
    async fn ws_memory_resolution_honors_agent_backend_none_over_install_backend() {
        use tempfile::TempDir;
        use zeroclaw_config::multi_agent::MemoryBackendKind;
        use zeroclaw_config::schema::{AliasedAgentConfig, Config};

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config.memory.backend = "sqlite.default".to_string();

        let mut agent = AliasedAgentConfig::default();
        agent.memory.backend = MemoryBackendKind::None;
        config.agents.insert("web".to_string(), agent);

        let memory = resolve_ws_memory_handle(&config, "web")
            .await
            .expect("WS per-agent memory resolution");

        assert!(
            memory.is_none(),
            "WebSocket consolidation must disable memory when the agent backend is none"
        );
    }

    #[test]
    fn event_matches_session_passes_session_scoped_chat_messages() {
        // /api/sessions/{id}/messages broadcasts a session-scoped assistant
        // injection — that frame must reach the chat for its session.
        let assistant_inject = serde_json::json!({
            "type": "message",
            "session_id": "operator-1",
            "role": "assistant",
            "content": "hello",
        });
        assert!(event_matches_session(&assistant_inject, "operator-1"));
        assert!(!event_matches_session(&assistant_inject, "operator-2"));
    }

    #[test]
    fn observability_tagged_frames_are_filtered() {
        // The defense-in-depth helper: any frame with source="observability"
        // is telemetry, regardless of type or session_id presence.
        let obs = serde_json::json!({
            "type": "tool_call",
            "source": "observability",
            "tool": "shell",
        });
        assert!(is_observability_telemetry(&obs));

        let chat = serde_json::json!({
            "type": "tool_call",
            "id": "call-1",
            "name": "file_write",
            "args": {"path": "/tmp/x"},
        });
        assert!(!is_observability_telemetry(&chat));
    }

    #[test]
    fn observability_telemetry_filter_handles_malformed_source_field() {
        // Edge cases the previous tool-frame discriminator covered: ensure
        // the source-tag check doesn't false-positive on weird `source`
        // values that happen to coexist with chat-shaped frames.
        for source in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!(42),
            serde_json::json!("api"),
            serde_json::json!({"nested": "x"}),
        ] {
            let frame = serde_json::json!({
                "type": "tool_call",
                "id": "call-1",
                "name": "file_write",
                "source": source,
            });
            assert!(
                !is_observability_telemetry(&frame),
                "frame with source={frame:?} must not be flagged as observability telemetry",
            );
        }
    }

    #[test]
    fn chat_tool_frames_pass_through_when_session_scoped() {
        // Real chat tool frames (ws.rs process_chat_message) are streamed
        // over the per-turn channel, not the broadcast bus, but if anything
        // ever rebroadcasts one with the right session_id it must pass.
        let chat_tool_call = serde_json::json!({
            "type": "tool_call",
            "session_id": "operator-1",
            "id": "call-1",
            "name": "file_write",
            "args": {"path": "/tmp/x"},
        });
        assert!(event_matches_session(&chat_tool_call, "operator-1"));
        assert!(!is_observability_telemetry(&chat_tool_call));
    }

    #[test]
    fn resolve_session_cwd_uses_requested_cwd() {
        let fallback = tempfile::tempdir().unwrap();
        let requested = fallback.path().join("project");
        std::fs::create_dir(&requested).unwrap();

        let resolved =
            resolve_session_cwd(Some(requested.to_str().unwrap()), fallback.path()).unwrap();

        assert_eq!(resolved, requested.canonicalize().unwrap());
    }

    #[test]
    fn resolve_session_cwd_uses_default_workspace_without_request() {
        let fallback = tempfile::tempdir().unwrap();

        let resolved = resolve_session_cwd(None, fallback.path()).unwrap();

        assert_eq!(resolved, fallback.path().canonicalize().unwrap());
    }

    #[test]
    fn resolve_ws_session_cwd_defaults_to_agent_workspace_without_request() {
        use tempfile::TempDir;
        use zeroclaw_config::schema::{AliasedAgentConfig, Config};

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config
            .agents
            .insert("web".to_string(), AliasedAgentConfig::default());
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let agent_workspace = config.agent_workspace_dir("web");
        assert!(!agent_workspace.exists());

        let resolved = resolve_ws_session_cwd(None, &config, "web").unwrap();

        assert!(agent_workspace.exists());
        assert_eq!(resolved, agent_workspace.canonicalize().unwrap());
        assert_ne!(resolved, config.data_dir.canonicalize().unwrap());
    }

    #[test]
    fn resolve_ws_session_cwd_keeps_requested_cwd_strict() {
        use tempfile::TempDir;
        use zeroclaw_config::schema::{AliasedAgentConfig, Config};

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config
            .agents
            .insert("web".to_string(), AliasedAgentConfig::default());
        let agent_workspace = config.agent_workspace_dir("web");
        let missing_requested = tmp.path().join("missing");

        let err = resolve_ws_session_cwd(Some(missing_requested.to_str().unwrap()), &config, "web")
            .expect_err("explicit missing cwd should be rejected");

        assert!(!agent_workspace.exists());
        assert!(err.to_string().contains("cwd is not a usable directory"));
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
    fn resolve_session_cwd_rejects_outside_boundaries() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("install");
        let workspace = install.join("agent/workspace");
        let sibling = install.join("agent/workspace-other");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        for outside in [
            workspace.ancestors().last().unwrap().to_path_buf(),
            install,
            workspace.parent().unwrap().to_path_buf(),
            workspace.join(".."),
            sibling,
        ] {
            let error = resolve_session_cwd(Some(outside.to_str().unwrap()), &workspace)
                .expect_err("outside cwd must be rejected");
            assert!(
                error
                    .to_string()
                    .contains("within the configured agent workspace")
            );
        }
        assert_eq!(
            resolve_session_cwd(Some(workspace.to_str().unwrap()), &workspace).unwrap(),
            workspace.canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_session_cwd_rejects_empty_and_file_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        std::fs::write(&file, "fixture").unwrap();
        for requested in ["", file.to_str().unwrap()] {
            assert!(resolve_session_cwd(Some(requested), tmp.path()).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_session_cwd_checks_canonical_symlink_targets() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let child = workspace.join("child");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let escape = workspace.join("escape");
        let inside_link = workspace.join("inside");
        let workspace_link = tmp.path().join("workspace-link");
        symlink(&outside, &escape).unwrap();
        symlink(&child, &inside_link).unwrap();
        symlink(&workspace, &workspace_link).unwrap();
        assert!(resolve_session_cwd(Some(escape.to_str().unwrap()), &workspace).is_err());
        assert_eq!(
            resolve_session_cwd(Some(inside_link.to_str().unwrap()), &workspace_link).unwrap(),
            child.canonicalize().unwrap()
        );
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

    /// Answers every call with `reply N`, recording how many messages the
    /// request carried. Holds each call until `gate` has a permit.
    struct ScriptedProvider {
        vision: bool,
        gate: Arc<tokio::sync::Semaphore>,
        side_query_gate: Option<Arc<SideQueryTestGate>>,
        seen: Arc<parking_lot::Mutex<Vec<Vec<String>>>>,
        systems: Arc<parking_lot::Mutex<Vec<String>>>,
        corrections:
            Arc<parking_lot::Mutex<Vec<Option<zeroclaw_api::review::OwnerCorrectionContext>>>>,
        correction_tool: Option<Arc<dyn zeroclaw_api::tool::Tool>>,
        correction_results: Arc<parking_lot::Mutex<Vec<bool>>>,
    }

    #[async_trait::async_trait]
    impl zeroclaw_api::model_provider::ModelProvider for ScriptedProvider {
        fn supports_vision(&self) -> bool {
            self.vision
        }
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            request: zeroclaw_providers::ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
            let is_side_query = request.messages.last().is_some_and(|m| {
                m.role == "user"
                    && m.content.starts_with(
                        zeroclaw_runtime::agent::side_query::BTW_SIDE_QUERY_INSTRUCTION,
                    )
            });
            if is_side_query {
                if let Some(gate) = &self.side_query_gate {
                    gate.entered.notify_one();
                    gate.release.acquire().await?.forget();
                }
            } else {
                self.gate.acquire().await?.forget();
                self.corrections.lock().push(
                    zeroclaw_api::review::OWNER_CORRECTION_CONTEXT
                        .try_with(|resolve| resolve())
                        .ok()
                        .flatten(),
                );
                if let Some(tool) = &self.correction_tool {
                    let result = tool
                        .execute(serde_json::json!({
                            "kind": "preference",
                            "statement": "Turn correction",
                            "semantic_key": "style"
                        }))
                        .await?;
                    self.correction_results.lock().push(result.success);
                }
            }
            self.systems.lock().extend(
                request
                    .messages
                    .iter()
                    .find(|m| m.role == "system")
                    .map(|m| m.content.clone()),
            );
            let mut seen = self.seen.lock();
            seen.push(
                request
                    .messages
                    .iter()
                    .filter(|m| m.role == "user")
                    .map(|m| m.content.clone())
                    .collect(),
            );
            // `echo:<text>` scripts the reply text itself.
            let echoed = seen
                .last()
                .and_then(|turn| turn.last())
                .and_then(|m| m.split_once("echo:"))
                .map(|(_, text)| text.to_string());
            Ok(zeroclaw_providers::ChatResponse {
                text: Some(echoed.unwrap_or_else(|| format!("reply {}", seen.len()))),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ScriptedProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "scripted"
        }
    }

    mod review_source_tests {
        include!("ws/review_source_tests.rs");
    }

    mod cancel_tests {
        include!("ws/cancel_tests.rs");
    }

    mod intake_tests {
        include!("ws/intake_tests.rs");
    }

    mod surface_tests {
        include!("ws/surface_tests.rs");
    }

    mod device_tests {
        include!("ws/device_tests.rs");
    }

    mod completion_tests {
        include!("ws/completion_tests.rs");
    }

    struct SideQueryTestGate {
        entered: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
    }

    struct SharedChat {
        vision: bool,
        state: AppState,
        scope: WsTurnScope,
        gate: Arc<tokio::sync::Semaphore>,
        side_query_gate: Option<Arc<SideQueryTestGate>>,
        seen: Arc<parking_lot::Mutex<Vec<Vec<String>>>>,
        systems: Arc<parking_lot::Mutex<Vec<String>>>,
        corrections:
            Arc<parking_lot::Mutex<Vec<Option<zeroclaw_api::review::OwnerCorrectionContext>>>>,
        correction_tool: Option<Arc<dyn zeroclaw_api::tool::Tool>>,
        correction_results: Arc<parking_lot::Mutex<Vec<bool>>>,
        /// When set, the agent is a body agent assembling Soul and User
        /// Model per turn from this config's `data_dir`.
        owner_config: Option<Arc<zeroclaw_config::schema::Config>>,
        _tmp: tempfile::TempDir,
    }

    impl SharedChat {
        fn new() -> Self {
            let tmp = tempfile::TempDir::new().unwrap();
            let state = crate::tests::admin_paircode_state(&tmp, false, false);
            Self {
                vision: false,
                state,
                scope: WsTurnScope {
                    session_key: "gw_shared".into(),
                    session_id: "shared".into(),
                    auth_subject: None,
                    surface: None,
                    bridge: None,
                },
                gate: Arc::new(tokio::sync::Semaphore::new(0)),
                side_query_gate: None,
                seen: Arc::default(),
                systems: Arc::default(),
                corrections: Arc::default(),
                correction_tool: None,
                correction_results: Arc::default(),
                owner_config: None,
                _tmp: tmp,
            }
        }

        async fn attach(&self) -> crate::ws_conversation::Subscription<WsSession> {
            self.attach_key(&self.scope.session_key).await
        }

        async fn attach_key(&self, key: &str) -> crate::ws_conversation::Subscription<WsSession> {
            let (subscription, _) = self
                .state
                .ws_conversations
                .attach(key, |_seed| {
                    let provider = ScriptedProvider {
                        vision: self.vision,
                        gate: Arc::clone(&self.gate),
                        side_query_gate: self.side_query_gate.clone(),
                        seen: Arc::clone(&self.seen),
                        systems: Arc::clone(&self.systems),
                        corrections: Arc::clone(&self.corrections),
                        correction_tool: self.correction_tool.clone(),
                        correction_results: Arc::clone(&self.correction_results),
                    };
                    let workspace = self._tmp.path().to_path_buf();
                    let owner_config = self.owner_config.clone();
                    async move {
                        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
                            backend: "none".into(),
                            ..Default::default()
                        };
                        let mem: Arc<dyn zeroclaw_memory::Memory> = Arc::from(
                            zeroclaw_memory::create_memory(&memory_cfg, &workspace, None).unwrap(),
                        );
                        let mut builder = zeroclaw_runtime::agent::Agent::builder()
                            .model_provider(Box::new(provider))
                            .tools(vec![])
                            .memory(mem)
                            .observer(Arc::new(zeroclaw_runtime::observability::NoopObserver))
                            .tool_dispatcher(Box::new(
                                zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher,
                            ))
                            .workspace_dir(workspace.clone())
                            .model_name("test-model".into())
                            .model_provider_name("scripted".into())
                            .agent_alias("web".into());
                        if let Some(config) = owner_config {
                            builder = builder
                                .multimodal_config(config.multimodal.clone())
                                .provider_switch_config(
                                    zeroclaw_runtime::agent::agent::ProviderSwitchConfig {
                                        config: Some(config),
                                    },
                                )
                                .governed_turn_context(None);
                        }
                        let mut agent = builder.build()?;
                        agent.add_prompt_section(Box::new(surface::Section));
                        Ok::<_, anyhow::Error>((
                            WsSession {
                                agent,
                                ws_memory: None,
                            },
                            (),
                        ))
                    }
                })
                .await
                .unwrap();
            subscription
        }

        fn send(
            &self,
            from: &crate::ws_conversation::Subscription<WsSession>,
            frame: serde_json::Value,
        ) -> Option<serde_json::Value> {
            let (reply, start) = handle_client_text(
                &self.state,
                &from.conversation,
                &self.scope,
                &frame.to_string(),
            );
            if let Some(claim) = start {
                start_ws_turns(&self.state, &from.conversation, &self.scope, claim);
            }
            reply
        }
    }

    #[tokio::test]
    async fn question_answers_require_live_authority_and_the_same_conversation() {
        let mut chat = SharedChat::new();
        let mut a = chat.attach().await;
        let owner_hash = zeroclaw_config::tokens::hash_token("question-owner-fixture");
        chat.state.config.write().gateway.bridges.insert(
            "telegram".into(),
            zeroclaw_config::schema::GatewayBridgeConfig {
                token_hash: owner_hash.clone(),
                sessions: vec!["shared".into()],
                session_prefix: None,
                owner: None,
                device: None,
            },
        );
        let port = super::super::ws_question::QuestionPort {
            questions: a.conversation.questions.clone(),
            frames: a.conversation.frame_sink(),
            config: chat.state.config.clone(),
        };
        let task = zeroclaw_spawn::spawn!(async move {
            port.ask("Name?", &[], Duration::from_secs(5))
                .await
                .unwrap()
        });
        let frame = a.frames.recv().await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let id = frame["request_id"].as_str().unwrap();
        let answer = serde_json::json!({"type":"answer", "request_id":id, "text":"owner answer"});
        assert_eq!(
            chat.send(&a, answer.clone()).unwrap()["status"],
            "unauthorized"
        );
        chat.scope.auth_subject = Some(owner_hash);
        chat.scope.session_key = "gw_other".into();
        let other = chat.attach().await;
        assert_eq!(
            chat.send(&other, answer.clone()).unwrap()["status"],
            "stale"
        );
        chat.state.config.write().gateway.bridges.clear();
        assert_eq!(
            chat.send(&a, answer.clone()).unwrap()["status"],
            "unauthorized"
        );
        chat.state.config.write().gateway.bridges.insert(
            "telegram".into(),
            zeroclaw_config::schema::GatewayBridgeConfig {
                token_hash: chat.scope.auth_subject.clone().unwrap(),
                sessions: vec!["shared".into()],
                session_prefix: None,
                owner: None,
                device: None,
            },
        );
        assert!(chat.send(&a, answer.clone()).is_none());
        assert_eq!(task.await.unwrap().as_deref(), Some("owner answer"));
        assert_eq!(chat.send(&a, answer).unwrap()["status"], "stale");
        let mut accepted = false;
        for _ in 0..2 {
            let frame = a.frames.recv().await.unwrap();
            let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
            accepted |= frame["type"] == "answer_ack" && frame["status"] == "accepted";
        }
        assert!(accepted);
    }

    /// Frames up to and including the first terminal one.
    async fn frames_until_end(
        sub: &mut crate::ws_conversation::Subscription<WsSession>,
    ) -> Vec<serde_json::Value> {
        let mut frames = Vec::new();
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), sub.frames.recv())
                .await
                .expect("turn must settle")
                .expect("frame");
            let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
            let end = matches!(frame["type"].as_str(), Some("done" | "aborted" | "error"));
            frames.push(frame);
            if end {
                return frames;
            }
        }
    }

    fn message(content: &str) -> serde_json::Value {
        serde_json::json!({ "type": "message", "content": content })
    }

    #[test]
    fn approval_request_frame_redacts_leaked_credentials() {
        let token = format!("zc_{}", "1a2b3c4d".repeat(8));
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        // Bare forms: the KV-shaped `token=…` pattern catches the assignment
        // shape; the frame boundary must catch the bare one too.
        let summary = format!("run shell for the owner: {token} and {key}");
        let frame = approval_request_ws_frame(
            "ap1",
            "shell",
            &summary,
            120,
            &zeroclaw_config::schema::LeakDetectionConfig::default(),
        );
        let text = serde_json::to_string(&frame).unwrap();
        assert!(!text.contains(&token), "{text}");
        assert!(!text.contains(key), "{text}");
        assert!(text.contains("[REDACTED"), "{text}");
    }

    #[test]
    fn history_trimmed_frame_redacts_leaked_credentials() {
        let token = format!("zcb_{}", "9f8e7d6c".repeat(8));
        let reason = format!("context over budget; last payload held {token}");
        let frame = history_trimmed_ws_frame(
            3,
            2,
            &reason,
            &zeroclaw_config::schema::LeakDetectionConfig::default(),
        );
        let text = serde_json::to_string(&frame).unwrap();
        assert!(!text.contains(&token), "{text}");
        assert!(text.contains("[REDACTED"), "{text}");
    }

    #[test]
    fn thinking_tool_and_plan_frames_redact_leaked_credentials() {
        let leak = zeroclaw_config::schema::LeakDetectionConfig::default();
        let token = format!("zc_{}", "deadbeef".repeat(8));
        for (label, frame) in [
            (
                "thinking",
                turn_event_ws_frame(
                    zeroclaw_api::agent::TurnEvent::Thinking {
                        delta: format!("the owner's key is {token}"),
                    },
                    &leak,
                ),
            ),
            (
                "tool_call",
                turn_event_ws_frame(
                    zeroclaw_api::agent::TurnEvent::ToolCall {
                        id: "t1".into(),
                        name: "shell".into(),
                        args: serde_json::json!({ "env": token, "flags": ["-l"] }),
                    },
                    &leak,
                ),
            ),
            (
                "tool_result",
                turn_event_ws_frame(
                    zeroclaw_api::agent::TurnEvent::ToolResult {
                        id: "t1".into(),
                        name: "shell".into(),
                        output: format!("exported {token}"),
                    },
                    &leak,
                ),
            ),
            (
                "plan",
                turn_event_ws_frame(
                    zeroclaw_api::agent::TurnEvent::Plan {
                        entries: vec![zeroclaw_api::plan::PlanEntry {
                            content: format!("rotate the key {token}"),
                            status: Default::default(),
                            priority: Default::default(),
                            active_form: None,
                        }],
                    },
                    &leak,
                ),
            ),
        ] {
            let text = serde_json::to_string(&frame).unwrap();
            assert!(!text.contains(&token), "{label}: {text}");
            assert!(text.contains("[REDACTED"), "{label}: {text}");
        }
    }

    #[tokio::test]
    async fn chunks_and_done_frame_are_redacted_and_stripped_like_channels() {
        let chat = SharedChat::new();
        let mut a = chat.attach().await;
        chat.gate.add_permits(1);
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCD";

        assert!(
            chat.send(
                &a,
                message(&format!(
                    "echo:<think>private plan</think>Your key is {key} \
                     <tool_call>{{\"name\":\"shell\",\"arguments\":{{}}}}</tool_call>done"
                )),
            )
            .is_none()
        );
        let frames = frames_until_end(&mut a).await;
        let done = frames.last().unwrap();
        assert_eq!(done["type"], "done");
        let full = done["full_response"].as_str().unwrap();
        assert!(!full.contains(key), "{full}");
        assert!(full.contains("[REDACTED"), "{full}");
        assert!(full.starts_with("Your key is "), "{full}");
        for hidden in ["private plan", "<think>", "tool_call", "\"shell\""] {
            assert!(!full.contains(hidden), "{hidden} in {full}");
        }
        let streamed: String = frames
            .iter()
            .filter(|f| f["type"] == "chunk")
            .filter_map(|f| f["content"].as_str())
            .collect();
        assert!(streamed.starts_with("Your key is "), "{streamed}");
        for hidden in ["sk-ant", "private plan", "<think>", "tool_call"] {
            assert!(!streamed.contains(hidden), "{hidden} in {streamed}");
        }
    }

    #[tokio::test]
    async fn sockets_on_one_session_share_its_turns_and_history() {
        let chat = SharedChat::new();
        let mut a = chat.attach().await;
        let mut b = chat.attach().await;
        chat.gate.add_permits(2);

        assert!(chat.send(&a, message("first")).is_none());
        for sub in [&mut a, &mut b] {
            let done = frames_until_end(sub).await.pop().unwrap();
            assert_eq!(done["type"], "done");
            assert_eq!(done["full_response"], "reply 1");
        }

        // The other socket continues the same history instead of a fork.
        assert!(chat.send(&b, message("second")).is_none());
        for sub in [&mut a, &mut b] {
            let done = frames_until_end(sub).await.pop().unwrap();
            assert_eq!(done["full_response"], "reply 2");
        }
        let history = chat.seen.lock()[1].clone();
        assert_eq!(history.len(), 2, "{history:?}");
        assert!(history[0].ends_with("first") && history[1].ends_with("second"));
    }

    /// #380 U3: the `/ws/chat` turn path carries the owner profile that
    /// applies to this session, and a Soul change reaches the live session on
    /// its next turn without a reconnect.
    #[tokio::test]
    async fn ws_turns_carry_the_owner_profile_and_follow_soul_changes() {
        use zeroclaw_memory::companion::{
            SoulIdentity, SoulProfileStore, UserModelKind, UserModelStore,
        };
        let mut chat = SharedChat::new();
        let data_dir = chat._tmp.path().join("owner-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let user_model = UserModelStore::shared(&data_dir).unwrap();
        for (statement, key, scope) in [
            ("Answer in English.", "lang", "global"),
            ("Focus on the trading plan.", "focus", "session:gw_shared"),
            ("Talk about gardening.", "hobby", "session:gw_other"),
        ] {
            user_model
                .record_owner_statement(UserModelKind::Preference, statement, key, scope, 1)
                .unwrap();
        }
        chat.owner_config = Some(Arc::new(zeroclaw_config::schema::Config {
            data_dir: data_dir.clone(),
            ..zeroclaw_config::schema::Config::default()
        }));
        let mut a = chat.attach().await;
        chat.gate.add_permits(2);

        assert!(chat.send(&a, message("first")).is_none());
        assert_eq!(
            frames_until_end(&mut a).await.pop().unwrap()["type"],
            "done"
        );
        let first = chat.systems.lock()[0].clone();
        assert!(
            first.contains("## Owner profile (authoritative)"),
            "{first}"
        );
        assert!(first.contains("Answer in English."), "{first}");
        assert!(first.contains("Focus on the trading plan."), "{first}");
        assert!(!first.contains("gardening"), "{first}");
        assert!(first.contains("You are web."), "{first}");

        let soul = SoulProfileStore::shared(&data_dir).unwrap();
        let head = soul.profile("web").unwrap().identity.unwrap().revision;
        soul.set_identity(
            "web",
            SoulIdentity {
                name: "Webby".into(),
                self_description: None,
                primary_language: None,
                pronouns: None,
            },
            head,
            2,
        )
        .unwrap();

        assert!(chat.send(&a, message("second")).is_none());
        assert_eq!(
            frames_until_end(&mut a).await.pop().unwrap()["type"],
            "done"
        );
        let second = chat.systems.lock()[1].clone();
        assert!(second.contains("You are Webby."), "{second}");
        assert!(!second.contains("You are web."), "{second}");
        assert!(second.contains("Focus on the trading plan."), "{second}");
    }

    #[tokio::test]
    async fn closing_a_socket_does_not_cancel_the_turn() {
        let chat = SharedChat::new();
        let a = chat.attach().await;
        let mut b = chat.attach().await;

        assert!(chat.send(&a, message("keep going")).is_none());
        drop(a);
        chat.gate.add_permits(1);

        let done = frames_until_end(&mut b).await.pop().unwrap();
        assert_eq!(done["type"], "done");
        assert_eq!(done["full_response"], "reply 1");
    }

    #[tokio::test]
    async fn a_cancel_frame_aborts_the_turn_for_every_socket() {
        let chat = SharedChat::new();
        let mut a = chat.attach().await;
        let mut b = chat.attach().await;

        assert!(chat.send(&a, message("long job")).is_none());
        while !a.conversation.is_running() {
            tokio::task::yield_now().await;
        }
        assert!(
            chat.send(&b, serde_json::json!({ "type": "cancel" }))
                .is_none()
        );
        chat.gate.add_permits(1);

        for sub in [&mut a, &mut b] {
            let end = frames_until_end(sub).await.pop().unwrap();
            assert_eq!(end["type"], "aborted");
        }
        let reply = chat
            .send(&a, serde_json::json!({ "type": "cancel" }))
            .unwrap();
        assert_eq!(reply["code"], "NO_ACTIVE_TURN");
    }

    #[tokio::test]
    async fn invalid_client_frames_are_answered_on_that_socket_only() {
        let chat = SharedChat::new();
        let a = chat.attach().await;
        for (frame, code) in [
            (
                serde_json::json!({ "type": "message", "content": "" }),
                "EMPTY_CONTENT",
            ),
            (
                serde_json::json!({ "type": "nope" }),
                "UNKNOWN_MESSAGE_TYPE",
            ),
            (
                serde_json::json!({ "type": "approval_response", "request_id": "r" }),
                "INVALID_APPROVAL_RESPONSE",
            ),
        ] {
            assert_eq!(chat.send(&a, frame).unwrap()["code"], code);
        }
        let (reply, start) = handle_client_text(&chat.state, &a.conversation, &chat.scope, "{");
        assert_eq!(reply.unwrap()["code"], "INVALID_JSON");
        assert!(start.is_none());
        assert!(!a.conversation.is_running());
    }

    fn message_with_id(content: &str, id: &str) -> serde_json::Value {
        serde_json::json!({ "type": "message", "content": content, "id": id })
    }

    #[tokio::test]
    async fn attachment_message_reaches_one_real_turn_and_retries_do_not_run_it_again() {
        let mut chat = SharedChat::new();
        chat.vision = true;
        chat.state.config.write().agents.insert(
            "web".into(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        chat.scope.session_key = "gw_shared\u{1f}web".into();
        let subject = zeroclaw_config::tokens::hash_token("synthetic-file-token");
        chat.scope.auth_subject = Some(subject.clone());
        chat.state.config.write().gateway.bridges.insert(
            "files".into(),
            zeroclaw_config::schema::GatewayBridgeConfig {
                token_hash: subject.clone(),
                sessions: vec!["shared".into()],
                ..Default::default()
            },
        );
        let scope = crate::api_attachments::Scope {
            subject,
            session: "shared".into(),
            agent: "web".into(),
        };
        let id = chat
            .state
            .ws_conversations
            .attachments
            .insert(
                scope.clone(),
                "note.txt".into(),
                "text/plain".into(),
                axum::body::Bytes::from_static(b"attachment contents"),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut a = chat.attach().await;
        chat.gate.add_permits(2);
        let frame =
            serde_json::json!({"type":"message","id":"with-file","content":"","attachments":[id]});
        assert_eq!(chat.send(&a, frame.clone()).unwrap()["status"], "accepted");
        assert_eq!(
            frames_until_end(&mut a).await.pop().unwrap()["type"],
            "done"
        );
        assert!(
            chat.seen.lock()[0]
                .iter()
                .any(|s| s.contains("attachment contents"))
        );
        // Lose the ACK and release the idle conversation with no backend.
        assert!(chat.state.session_backend.is_none());
        let old = a.conversation.clone();
        drop(a);
        chat.state.ws_conversations.release_if_unused(&old);
        let mut a = chat.attach().await;
        assert!(!Arc::ptr_eq(&old, &a.conversation));
        assert_eq!(chat.send(&a, frame).unwrap()["status"], "duplicate");
        assert_eq!(chat.seen.lock().len(), 1);
        let invalid = serde_json::json!({"type":"message","id":"unknown-file","content":"read","attachments":[uuid::Uuid::new_v4().to_string()]});
        assert_eq!(
            chat.send(&a, invalid).unwrap()["code"],
            "ATTACHMENT_UNAVAILABLE"
        );
        assert!(!a.conversation.is_running());
        let image = chat
            .state
            .ws_conversations
            .attachments
            .insert(
                scope,
                "photo.png".into(),
                "image/png".into(),
                axum::body::Bytes::from_static(b"\x89PNG\r\n\x1a\nfixture"),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(chat.send(&a,serde_json::json!({"type":"message","id":"image","content":"describe","attachments":[image]})).unwrap()["status"],"accepted");
        assert_eq!(
            frames_until_end(&mut a).await.pop().unwrap()["type"],
            "done"
        );
        assert!(
            chat.seen.lock()[1]
                .iter()
                .any(|s| s.contains("[IMAGE:data:image/png;base64,"))
        );
        chat.state.config.write().gateway.bridges.clear();
        assert_eq!(chat.send(&a,serde_json::json!({"type":"message","id":"revoked","content":"read","attachments":[image]})).unwrap()["code"],"UNAUTHORIZED_ATTACHMENTS");
        assert!(!a.conversation.is_running());
    }

    #[tokio::test]
    async fn durable_attachment_receipt_survives_capacity_pressure_and_reconnect() {
        let mut chat = SharedChat::new();
        chat.state.session_backend = Some(Arc::new(
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(chat._tmp.path()).unwrap(),
        ));
        chat.state.config.write().agents.insert(
            "web".into(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        chat.scope.session_key = "gw_shared\u{1f}web".into();
        let subject = zeroclaw_config::tokens::hash_token("synthetic-file-token");
        chat.scope.auth_subject = Some(subject.clone());
        chat.state.config.write().gateway.bridges.insert(
            "files".into(),
            zeroclaw_config::schema::GatewayBridgeConfig {
                token_hash: subject.clone(),
                sessions: vec!["shared".into()],
                ..Default::default()
            },
        );
        let handle = chat
            .state
            .ws_conversations
            .attachments
            .insert(
                crate::api_attachments::Scope {
                    subject,
                    session: "shared".into(),
                    agent: "web".into(),
                },
                "note.txt".into(),
                "text/plain".into(),
                axum::body::Bytes::from_static(b"capacity attachment"),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        chat.gate.add_permits(1);
        let mut a = chat.attach().await;
        let frame = serde_json::json!({"type":"message","id":"lost-file-ack","content":"read","attachments":[handle]});
        assert_eq!(chat.send(&a, frame.clone()).unwrap()["durable"], true);
        assert_eq!(
            frames_until_end(&mut a).await.pop().unwrap()["type"],
            "done"
        );
        let backend = chat.state.session_backend.as_ref().unwrap();
        for i in 0..255 {
            backend
                .record_request(&chat.scope.session_key, &format!("other-{i}"), "rejected")
                .unwrap();
        }
        assert!(
            backend
                .record_request(&chat.scope.session_key, "overflow", "accepted")
                .is_err()
        );
        let old = a.conversation.clone();
        drop(a);
        chat.state.ws_conversations.release_if_unused(&old);
        let a = chat.attach().await;
        assert!(!Arc::ptr_eq(&old, &a.conversation));
        assert_eq!(chat.send(&a, frame).unwrap()["status"], "duplicate");
        assert_eq!(chat.seen.lock().len(), 1);
        assert!(!a.conversation.is_running());
    }

    #[tokio::test]
    async fn a_message_id_is_acked_and_a_resend_is_not_run_again() {
        let chat = SharedChat::new();
        let mut a = chat.attach().await;
        chat.gate.add_permits(2);

        let ack = chat.send(&a, message_with_id("hello", "req-1")).unwrap();
        assert_eq!(ack["type"], "ack");
        assert_eq!(ack["id"], "req-1");
        assert_eq!(ack["status"], "accepted");
        assert_eq!(ack["turn"], "started");
        assert_eq!(ack["durable"], false, "no session store: memory only");
        let done = frames_until_end(&mut a).await.pop().unwrap();
        assert_eq!(done["type"], "done");
        assert_eq!(done["id"], "req-1");

        // A client that lost the ACK resends: it learns the outcome, and
        // nothing runs twice.
        let again = chat.send(&a, message_with_id("hello", "req-1")).unwrap();
        assert_eq!(again["status"], "duplicate");
        assert_eq!(again["state"], "done");
        assert!(!a.conversation.is_running());
        assert_eq!(chat.seen.lock().len(), 1);
    }

    #[tokio::test]
    async fn receipts_in_the_session_store_outlive_the_conversation() {
        let mut chat = SharedChat::new();
        chat.state.session_backend = Some(Arc::new(
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(chat._tmp.path()).unwrap(),
        ));
        chat.gate.add_permits(1);
        {
            let mut a = chat.attach().await;
            let ack = chat.send(&a, message_with_id("hello", "req-1")).unwrap();
            assert_eq!(ack["durable"], true);
            assert_eq!(
                frames_until_end(&mut a).await.pop().unwrap()["type"],
                "done"
            );
        }
        // Let the finished turn release the conversation.
        while chat.state.ws_conversations.len() != 0 {
            tokio::task::yield_now().await;
        }

        let b = chat.attach().await;
        let again = chat.send(&b, message_with_id("hello", "req-1")).unwrap();
        assert_eq!(again["status"], "duplicate");
        assert_eq!(again["state"], "done");
        assert_eq!(chat.seen.lock().len(), 1);
    }

    #[tokio::test]
    async fn a_message_id_during_a_turn_is_acked_as_steering() {
        let chat = SharedChat::new();
        let mut a = chat.attach().await;
        assert_eq!(
            chat.send(&a, message_with_id("first", "req-1")).unwrap()["turn"],
            "started"
        );
        let steer = chat
            .send(&a, message_with_id("also this", "req-2"))
            .unwrap();
        assert_eq!(steer["status"], "accepted");
        assert_eq!(steer["turn"], "steered");
        chat.gate.add_permits(4);
        let done = frames_until_end(&mut a).await.pop().unwrap();
        assert_eq!(done["id"], "req-1");
        let again = chat
            .send(&a, message_with_id("also this", "req-2"))
            .unwrap();
        assert_eq!(again["state"], "steered");
    }

    #[tokio::test]
    async fn bad_message_ids_are_refused_and_errors_carry_the_id() {
        let chat = SharedChat::new();
        let a = chat.attach().await;
        let too_long = "x".repeat(MAX_REQUEST_ID_LEN + 1);
        for id in [
            serde_json::json!(""),
            serde_json::json!(7),
            serde_json::json!(too_long),
            serde_json::json!("a\nb"),
        ] {
            let frame = serde_json::json!({ "type": "message", "content": "hi", "id": id });
            assert_eq!(chat.send(&a, frame).unwrap()["code"], "INVALID_REQUEST_ID");
        }
        let empty = chat.send(&a, message_with_id("", "req-9")).unwrap();
        assert_eq!(empty["code"], "EMPTY_CONTENT");
        assert_eq!(empty["id"], "req-9");
        assert!(!a.conversation.is_running());
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

    struct DeletedSessionBackend {
        append_calls: std::sync::Mutex<Vec<String>>,
    }

    impl zeroclaw_infra::session_backend::SessionBackend for DeletedSessionBackend {
        fn load(&self, _session_key: &str) -> Vec<zeroclaw_providers::ChatMessage> {
            Vec::new()
        }
        fn append(
            &self,
            session_key: &str,
            message: &zeroclaw_providers::ChatMessage,
        ) -> std::io::Result<()> {
            self.append_calls.lock().unwrap().push(format!(
                "{}:{}:{}",
                session_key, message.role, message.content
            ));
            Ok(())
        }
        fn remove_last(&self, _session_key: &str) -> std::io::Result<bool> {
            Ok(false)
        }
        fn list_sessions(&self) -> Vec<String> {
            Vec::new()
        }
        fn session_exists(&self, _session_key: &str) -> bool {
            // The user deleted the session between cancel and append.
            false
        }
    }

    #[test]
    fn persist_conversation_messages_skips_deleted_session() {
        use zeroclaw_providers::{ChatMessage, ConversationMessage};
        let backend = DeletedSessionBackend {
            append_calls: std::sync::Mutex::new(Vec::new()),
        };
        let messages = vec![
            ConversationMessage::Chat(ChatMessage::user("hi")),
            ConversationMessage::Chat(ChatMessage::assistant("[interrupted by user]")),
        ];

        persist_conversation_messages(&backend, "gw_deleted", &messages, None);

        assert!(
            backend.append_calls.lock().unwrap().is_empty(),
            "persist_conversation_messages must not resurrect a session whose \
             session_exists() returned false (see #7126)"
        );
    }

    #[test]
    fn stamp_session_keeps_the_name_of_a_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let backend = zeroclaw_infra::make_session_backend(dir.path(), "sqlite").unwrap();
        let reported = stamp_session(backend.as_ref(), "gw_new1", "default", Some("Foo"));
        assert_eq!(reported.as_deref(), Some("Foo"));
        assert_eq!(
            backend.get_session_name("gw_new1").unwrap().as_deref(),
            Some("Foo"),
            "the name of a session created on connect must be stored"
        );
        // Reconnecting without a name reports the stored one.
        let reported = stamp_session(backend.as_ref(), "gw_new1", "default", None);
        assert_eq!(reported.as_deref(), Some("Foo"));
    }

    #[test]
    fn parse_btw_command_recognizes_slash_btw_and_ignores_other_prefixes() {
        assert_eq!(
            parse_btw_command("/btw what is running?"),
            Some("what is running?")
        );
        assert_eq!(
            parse_btw_command("   /btw   status check  "),
            Some("status check")
        );
        assert_eq!(parse_btw_command("/btw\nmultiline"), Some("multiline"));
        assert_eq!(parse_btw_command("/btw"), Some(""));
        assert_eq!(parse_btw_command("/btw   "), Some(""));
        assert_eq!(parse_btw_command("/btwx not a btw command"), None);
        assert_eq!(parse_btw_command("regular message /btw inline"), None);
    }

    #[tokio::test]
    async fn btw_side_query_answers_during_blocked_turn_without_steering_or_history_mutation() {
        let mut chat = SharedChat::new();
        chat.state.pairing = Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
            false,
            &["owner-btw-token".into()],
        ));
        chat.scope.auth_subject = Some(zeroclaw_config::tokens::hash_token("owner-btw-token"));
        chat.state.session_backend = Some(Arc::new(
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(chat._tmp.path()).unwrap(),
        ));
        chat.state
            .session_backend
            .as_ref()
            .unwrap()
            .set_session_agent_alias(&chat.scope.session_key, "web")
            .unwrap();
        let mut a = chat.attach().await;

        // Start a main turn with 0 gate permits so the turn holds `conversation.agent`
        // and blocks inside `ScriptedProvider::chat`.
        let main_ack = chat
            .send(&a, message_with_id("main turn work", "main-1"))
            .unwrap();
        assert_eq!(main_ack["status"], "accepted");
        assert_eq!(main_ack["turn"], "started");
        assert!(a.conversation.is_running());

        // Yield briefly so the spawned turn task acquires `conversation.agent`,
        // updates the live side-query snapshot with the in-flight user message,
        // and reaches `self.gate.acquire().await`.
        for _ in 0..20 {
            if a.conversation
                .side_query
                .as_ref()
                .and_then(|sq| sq.active_turn_id())
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }

        // Send a `/btw` slash command over a regular `message` frame while the main turn is blocked.
        let btw_ack = chat
            .send(
                &a,
                message_with_id("/btw echo:side answer while busy", "btw-1"),
            )
            .unwrap();
        assert_eq!(btw_ack["type"], "ack");
        assert_eq!(btw_ack["id"], "btw-1");
        assert_eq!(btw_ack["status"], "accepted");
        assert_eq!(btw_ack["turn"], "btw");
        assert_eq!(btw_ack["durable"], false);

        let frame = tokio::time::timeout(Duration::from_secs(5), a.frames.recv())
            .await
            .expect("/btw must complete while the main turn is still blocked on the gate")
            .expect("btw_response frame");
        let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(frame["type"], "btw_response");
        assert_eq!(frame["id"], "btw-1");
        assert_eq!(frame["question"], "echo:side answer while busy");
        assert_eq!(frame["content"], "side answer while busy");
        assert_eq!(frame["full_response"], "side answer while busy");
        assert!(
            frame["turn_id"].as_str().is_some(),
            "btw_response during an active turn must report the active turn_id"
        );

        // Also verify explicit `{"type":"btw","content":"..."}` frame works while blocked.
        assert!(
            chat.send(
                &a,
                serde_json::json!({
                    "type": "btw",
                    "content": "echo:second side answer"
                }),
            )
            .is_none()
        );
        let frame2 = tokio::time::timeout(Duration::from_secs(5), a.frames.recv())
            .await
            .expect("explicit btw frame must complete while blocked")
            .expect("second btw_response frame");
        let frame2: serde_json::Value = serde_json::from_str(&frame2).unwrap();
        assert_eq!(frame2["type"], "btw_response");
        assert_eq!(frame2["content"], "second side answer");

        // Also verify S2 `turn_control` negotiated socket accepts `/btw` while the main turn is blocked.
        let mut turn_negotiated = false;
        let mut steering_negotiated = false;
        let (neg_reply, _) = turn_control::handle(
            &chat.state,
            &a.conversation,
            &chat.scope,
            &mut turn_negotiated,
            &mut steering_negotiated,
            &serde_json::json!({
                "type": "turn_control",
                "version": 1,
                "op": "negotiate"
            })
            .to_string(),
        );
        let epoch = neg_reply.expect("turn_ready")["epoch"]
            .as_str()
            .unwrap()
            .to_string();
        let s2_req_id = uuid::Uuid::new_v4().to_string();
        let s2_payload = serde_json::json!({
            "type": "turn_control",
            "version": 1,
            "op": "message",
            "epoch": epoch,
            "request_id": s2_req_id,
            "content": "/btw echo:s2 side answer"
        })
        .to_string();
        let (s2_reply, s2_start) = turn_control::handle(
            &chat.state,
            &a.conversation,
            &chat.scope,
            &mut turn_negotiated,
            &mut steering_negotiated,
            &s2_payload,
        );
        assert!(
            s2_start.is_none(),
            "/btw on turn_control must not start a turn"
        );
        let s2_reply = s2_reply.expect("turn_control ack");
        assert_eq!(s2_reply["type"], "turn_ack");
        assert_eq!(s2_reply["request_id"], s2_req_id);
        assert_eq!(s2_reply["status"], "btw");

        let s2_frame = tokio::time::timeout(Duration::from_secs(5), a.frames.recv())
            .await
            .expect("s2 btw_response while blocked")
            .expect("frame");
        let s2_frame: serde_json::Value = serde_json::from_str(&s2_frame).unwrap();
        assert_eq!(s2_frame["type"], "btw_response");
        assert_eq!(s2_frame["id"], s2_req_id);
        assert_eq!(s2_frame["content"], "s2 side answer");

        // On a negotiated S2 socket, a legacy `message` frame carrying `source`
        // or `attachments` with a `/btw` prefix must be rejected with
        // `UNKNOWN_OPERATION` and never reach `intake::receive`.
        let (spoofed_reply, spoofed_claim) = turn_control::handle(
            &chat.state,
            &a.conversation,
            &chat.scope,
            &mut turn_negotiated,
            &mut steering_negotiated,
            &serde_json::json!({
                "type": "message",
                "source": {"channel": "telegram", "sender_id": "u1"},
                "content": "/btw spoofed intake"
            })
            .to_string(),
        );
        assert!(spoofed_claim.is_none());
        let spoofed_reply = spoofed_reply.expect("spoofed source rejected");
        assert_eq!(spoofed_reply["type"], "turn_error");
        assert_eq!(spoofed_reply["code"], "UNKNOWN_OPERATION");

        // Now release the main turn and wait for its terminal `done` frame.
        chat.gate.add_permits(1);
        let done = frames_until_end(&mut a).await.pop().unwrap();
        assert_eq!(done["type"], "done");
        assert_eq!(done["id"], "main-1");

        // Verify the main turn's owner correction context was NOT invalidated by `/btw`
        // (unlike steering messages).
        {
            let corrections = chat.corrections.lock();
            assert_eq!(corrections.len(), 1);
            assert!(
                corrections[0].is_some(),
                "/btw must never invalidate owner correction authority on the running turn"
            );
        }

        // Verify neither `Agent::history` nor durable `session_backend` contains `/btw` messages.
        {
            let session = a.conversation.agent.lock().await;
            for msg in session.agent.history() {
                if let zeroclaw_providers::ConversationMessage::Chat(c) = msg {
                    assert!(
                        !c.content.contains("side answer") && !c.content.contains("/btw"),
                        "Agent::history must not contain /btw question or answer: {}",
                        c.content
                    );
                }
            }
        }
        let persisted = chat
            .state
            .session_backend
            .as_ref()
            .unwrap()
            .load(&chat.scope.session_key);
        assert_eq!(
            persisted.len(),
            2,
            "session store must only contain the main user+assistant turn"
        );
        assert!(
            persisted
                .iter()
                .all(|m| !m.content.contains("side answer") && !m.content.contains("/btw"))
        );
    }

    #[tokio::test]
    async fn session_delete_invalidates_in_flight_btw_and_allows_a_fresh_query() {
        const TOKEN: &str = "owner-delete-btw-fixture";
        let gate = Arc::new(SideQueryTestGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        let mut chat = SharedChat::new();
        chat.side_query_gate = Some(gate.clone());
        chat.state.pairing = Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
            false,
            &[TOKEN.into()],
        ));
        chat.scope.auth_subject = Some(zeroclaw_config::tokens::hash_token(TOKEN));
        chat.state.session_backend = Some(Arc::new(
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(chat._tmp.path()).unwrap(),
        ));
        let conversation_key = format!("{}\u{1f}web", chat.scope.session_key);
        let backend = chat.state.session_backend.as_deref().unwrap();
        stamp_session(backend, &chat.scope.session_key, "web", None);
        let mut subscriber = chat.attach_key(&conversation_key).await;
        chat.gate.add_permits(1);
        let ack = chat
            .send(&subscriber, message_with_id("deleted history", "main"))
            .unwrap();
        assert_eq!(ack["status"], "accepted");
        let frames = frames_until_end(&mut subscriber).await;
        assert_eq!(frames.last().unwrap()["type"], "done");
        let history = backend.load(&chat.scope.session_key);
        assert_eq!(history[0].role, "user");
        assert!(history[0].content.ends_with("] deleted history"));

        let side_query = subscriber.conversation.side_query.clone().unwrap();
        let permit = side_query.try_acquire_slot().unwrap();
        let old = spawn_ws_btw_query(
            &chat.state,
            &subscriber.conversation,
            side_query,
            permit,
            "echo:old side answer".into(),
            Some("old".into()),
        );
        let limit = Duration::from_secs(5);
        tokio::time::timeout(limit, gate.entered.notified())
            .await
            .expect("provider must hold the query before deletion");

        drop(subscriber);
        assert_eq!(chat.state.ws_conversations.len(), 1);

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().unwrap(),
        );
        let response = crate::api::handle_api_session_delete(
            axum::extract::State(chat.state.clone()),
            headers,
            axum::extract::Path(chat.scope.session_id.clone()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert!(!backend.session_exists(&chat.scope.session_key));
        tokio::time::timeout(limit, old)
            .await
            .expect("deletion must cancel the provider without waiting for its response")
            .unwrap();
        assert_eq!(chat.state.ws_conversations.len(), 0);

        gate.release.add_permits(1);
        let mut subscriber = chat.attach_key(&conversation_key).await;
        let ack = chat
            .send(
                &subscriber,
                message_with_id("/btw echo:fresh answer", "fresh"),
            )
            .unwrap();
        assert_eq!(ack["status"], "accepted");
        let frame = tokio::time::timeout(limit, subscriber.frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(frame.is_current(subscriber.conversation.reset_revision()));
        let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(frame["type"], "btw_response");
        assert_eq!(frame["id"], "fresh");
        assert_eq!(frame["content"], "fresh answer");
        assert_eq!(chat.seen.lock().last().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn btw_side_query_sanitizes_outbound_leaks_and_rejects_invalid_inputs() {
        let chat = SharedChat::new();
        let mut a = chat.attach().await;

        // Empty `/btw` is rejected without starting a turn.
        let empty_err = chat
            .send(&a, message_with_id("/btw   ", "btw-empty"))
            .unwrap();
        assert_eq!(empty_err["type"], "error");
        assert_eq!(empty_err["code"], "EMPTY_CONTENT");
        assert_eq!(empty_err["id"], "btw-empty");
        assert!(!a.conversation.is_running());

        // Attachments on `/btw` are rejected.
        let attach_err = chat
            .send(
                &a,
                serde_json::json!({
                    "type": "btw",
                    "id": "btw-att",
                    "content": "what is this?",
                    "attachments": [uuid::Uuid::new_v4().to_string()]
                }),
            )
            .unwrap();
        assert_eq!(attach_err["type"], "error");
        assert_eq!(attach_err["code"], "INVALID_ATTACHMENTS");
        assert_eq!(attach_err["id"], "btw-att");

        // Outbound response sanitization strips `<think>`, `<tool_call>`, and leaked tokens.
        let token = format!("zc_{}", "deadbeef".repeat(8));
        assert!(
            chat.send(
                &a,
                message(&format!(
                    "/btw echo:<think>secret plan</think>Token is {token} <tool_call>{{\"name\":\"shell\",\"arguments\":{{}}}}</tool_call>clean tail"
                )),
            )
            .is_none()
        );
        let frame = tokio::time::timeout(Duration::from_secs(5), a.frames.recv())
            .await
            .expect("btw_response")
            .expect("frame");
        let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(frame["type"], "btw_response");
        let echoed_question = frame["question"].as_str().unwrap();
        assert!(
            !echoed_question.contains(&token),
            "leaked token in echoed question must be redacted: {echoed_question}"
        );
        let content = frame["content"].as_str().unwrap();
        assert!(
            !content.contains(&token),
            "leaked token must be redacted: {content}"
        );
        assert!(
            !content.contains("<think>"),
            "thinking tags must be stripped: {content}"
        );
        assert!(
            !content.contains("<tool_call>"),
            "tool_call markup must be stripped: {content}"
        );
        assert!(
            content.contains("[REDACTED"),
            "redaction marker expected: {content}"
        );
    }
}
