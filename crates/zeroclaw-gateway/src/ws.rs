//! WebSocket agent chat handler.
//!
//! Approval summaries are operator-facing strings produced by the runtime's
//! key-name redaction heuristic. Approval decisions bind to `request_id`; this
//! transport forwards the summary without rebuilding it from raw arguments.

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
use zeroclaw_runtime::sop::approval::{
    ApprovalDecision as SopApprovalDecision, ApprovalPrincipal as SopApprovalPrincipal,
};

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
    // Auth: check header, subprotocol, then query param (precedence order). On
    // success derive a STABLE transport-authenticated subject (the paired-token
    // hash) so a required-group approval policy can be satisfied over WS; an
    // operator grants approval rights to this paired device via a `ws:<token-hash>`
    // group member. `None` when pairing is not required (no auth identity).
    let auth_subject = if state.pairing.require_pairing() {
        let token = extract_ws_token(&headers, params.token.as_deref()).unwrap_or("");
        match state.pairing.authenticate_and_hash(token) {
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
        None
    };

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
            auth_subject,
        )
    })
    .into_response()
}

/// Gateway session key prefix to avoid collisions with channel sessions.
const GW_SESSION_PREFIX: &str = "gw_";

fn websocket_ping_interval(
    config: &zeroclaw_config::schema::Config,
) -> Option<tokio::time::Interval> {
    let seconds = config.gateway.websocket_ping_interval_secs;
    if seconds == 0 {
        return None;
    }

    let period = Duration::from_secs(seconds);
    let start = tokio::time::Instant::now().checked_add(period)?;
    let mut interval = tokio::time::interval_at(start, period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    Some(interval)
}

async fn tick_websocket_ping(interval: &mut Option<tokio::time::Interval>) {
    if let Some(interval) = interval.as_mut() {
        interval.tick().await;
    } else {
        std::future::pending::<()>().await;
    }
}

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

async fn handle_ws_sop_frame<S>(
    parsed: &serde_json::Value,
    state: &AppState,
    session_id: &str,
    auth_subject: Option<&str>,
    sender: &mut S,
) -> bool
where
    S: SinkExt<Message> + Unpin,
{
    if parsed["kind"].as_str() != Some("sop") {
        return false;
    }
    let run_id = parsed["run_id"].as_str().unwrap_or("").to_string();
    let decision = match parsed["decision"].as_str().unwrap_or("") {
        "approve" => Some(SopApprovalDecision::Approve),
        // Thread the optional reason through, like the HTTP/CLI deny surfaces, so
        // the ledger records it.
        "deny" => Some(SopApprovalDecision::Deny {
            reason: parsed["reason"].as_str().map(str::to_string),
        }),
        _ => None,
    };
    // run_id + a valid decision are both required; the let-else avoids an expect
    // on the downstream resolve (codebase rule: no expect/unwrap in production).
    let Some(decision) = decision.filter(|_| !run_id.is_empty()) else {
        let err = serde_json::json!({
            "type": "error",
            "message": zeroclaw_runtime::i18n::get_required_cli_string(
                "cli-sop-ws-invalid-approval"
            ),
            "code": "INVALID_APPROVAL_RESPONSE"
        });
        let _ = sender.send(Message::Text(err.to_string().into())).await;
        return true;
    };
    let frame = if let Some(engine) = state.sop_engine.as_ref() {
        let principal =
            SopApprovalPrincipal::ws(session_id.to_string(), auth_subject.map(str::to_string));
        // EPIC G: route through the broker (membership + quorum); with no
        // `[sop.approval]` policy this is exactly `resolve_gate`.
        let resolved = match engine.lock() {
            Ok(mut g) => Some(g.resolve_via_broker_deferred(&run_id, decision, principal)),
            Err(_) => None,
        };
        match resolved {
            Some(Ok(outcome)) => {
                let config = state.config.read();
                zeroclaw_runtime::sop::drive_resumed_broker_action(
                    &config,
                    std::sync::Arc::clone(engine),
                    state.sop_audit.clone(),
                    &outcome,
                );
                serde_json::json!({
                    "type": "sop_approval_result",
                    "run_id": run_id,
                    "outcome": outcome.label(),
                })
            }
            Some(Err(e)) => serde_json::json!({
                "type": "error",
                "message": zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                    "cli-sop-ws-resolve-failed",
                    &[("error", &e.to_string())],
                ),
                "code": "SOP_RESOLVE_FAILED"
            }),
            None => serde_json::json!({
                "type": "error",
                "message": zeroclaw_runtime::i18n::get_required_cli_string(
                    "cli-sop-ws-engine-lock-poisoned"
                ),
                "code": "SOP_LOCK_POISONED"
            }),
        }
    } else {
        serde_json::json!({
            "type": "error",
            "message": zeroclaw_runtime::i18n::get_required_cli_string(
                "cli-sop-ws-subsystem-disabled"
            ),
            "code": "SOP_DISABLED"
        })
    };
    let _ = sender.send(Message::Text(frame.to_string().into())).await;
    true
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    agent_alias: String,
    session_id: Option<String>,
    session_name: Option<String>,
    session_cwd: Option<String>,
    // The transport-authenticated approval subject (paired-token hash), if the
    // connection was authenticated. Threaded to SOP approval frames so a policied
    // gate can be satisfied by an identified WS caller.
    auth_subject: Option<String>,
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
    let ws_memory = match resolve_ws_memory_handle(&config, &agent_alias).await {
        Ok(memory) => memory,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "agent": &agent_alias,
                        "error": format!("{e:#}"),
                    })),
                "WS per-agent memory resolution failed; consolidation disabled for connection"
            );
            None
        }
    };
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

    let mut first_msg_fallback: Option<String> = None;
    let mut requested_cwd = session_cwd;
    let mut ping_interval = websocket_ping_interval(&config);

    loop {
        let first = tokio::select! {
            first = receiver.next() => first,
            _ = tick_websocket_ping(&mut ping_interval) => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
                continue;
            }
        };

        match first {
            Some(Ok(Message::Text(text))) => {
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
                break;
            }
            Some(Ok(Message::Ping(payload))) => {
                if sender.send(Message::Pong(payload)).await.is_err() {
                    return;
                }
            }
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
            Some(Ok(_)) => {}
        }
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

    let mut agent =
        match zeroclaw_runtime::agent::Agent::from_live_config_with_session_cwd_and_mcp_backchannel(
            Arc::clone(&state.config),
            &agent_alias,
            Some(&session_cwd),
            true,
            false,
            // The gateway WebSocket turn does not transport ACP file attachments.
            false,
            state.sop_engine.clone(),
            state.sop_audit.clone(),
            Some(state.canvas_store.clone()),
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
        agent.seed_history_with_event(&stored_messages)
    };
    // How many persisted messages this connection's history already reflects.
    // Checked against the transcript under the session permit before every
    // turn, so a socket that reconnected behind a detached turn does not run
    // on the snapshot it loaded above.
    let mut persisted_watermark = stored_messages.len();

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
        .register_channel(WS_CHANNEL_KEY, approval_channel.clone());

    let ch = agent.channel_handles();
    let channel_names = zeroclaw_channels::orchestrator::register_channels_for_tools(
        &config,
        &ch.ask_user,
        &ch.channel_room,
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

    // Seeding happens before the connection's agent setup is complete. Forward
    // its one-shot trim outcome only after channels are registered, so restore
    // notifications cannot race setup or be emitted twice.
    if let Some(zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
        dropped_messages,
        kept_turns,
        reason,
    }) = restore_trim_event
    {
        let frame = history_trimmed_ws_frame(dropped_messages, kept_turns, &reason);
        let _ = sender.send(Message::Text(frame.to_string().into())).await;
    }

    // Process the first message if it was not a connect frame
    if let Some(ref text) = first_msg_fallback {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
            if parsed["type"].as_str() == Some("message") {
                if let Some(content) = first_chat_message_content(text) {
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
                    let client_gone = process_chat_message(
                        &state,
                        &mut agent,
                        &mut sender,
                        &mut receiver,
                        &mut approval_event_rx,
                        &pending_approvals,
                        &mut ping_interval,
                        &ws_memory,
                        &mut persisted_watermark,
                        &content,
                        &session_key,
                        &session_id,
                        auth_subject.as_deref(),
                    )
                    .await;
                    if client_gone {
                        return;
                    }
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
            // ── Keepalive ─────────────────────────────────────────────
            _ = tick_websocket_ping(&mut ping_interval) => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }

            // ── Client message ────────────────────────────────────────
            client_msg = receiver.next() => {
                let Some(msg) = client_msg else { break };
                let msg = match msg {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Ping(payload)) => {
                        if sender.send(Message::Pong(payload)).await.is_err() { break; }
                        continue;
                    }
                    Ok(Message::Pong(_)) => continue,
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
                    // EPIC C: a SOP-kind frame resolves a SOP gate via the shared
                    // engine + resolve_gate (keyed by run_id), NOT the tool-prompt
                    // pending_approvals map (keyed by request_id). The principal is
                    // transport-derived (ws + session id), never from the frame.
                    if handle_ws_sop_frame(
                        &parsed,
                        &state,
                        &session_id,
                        auth_subject.as_deref(),
                        &mut sender,
                    )
                    .await
                    {
                        continue;
                    }
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

                let client_gone = process_chat_message(
                    &state,
                    &mut agent,
                    &mut sender,
                    &mut receiver,
                    &mut approval_event_rx,
                    &pending_approvals,
                    &mut ping_interval,
                    &ws_memory,
                    &mut persisted_watermark,
                    &content,
                    &session_key,
                    &session_id,
                    auth_subject.as_deref(),
                )
                .await;
                if client_gone {
                    break;
                }
            }

            // ── Broadcast event (cron/heartbeat results) ──────────────
            event = broadcast_rx.recv() => {
                if let Ok(event) = event
                    && event_matches_session(&event, &session_id)
                    && !is_observability_telemetry(&event)
                {
                    let _ = sender.send(Message::Text(event.to_string().into())).await;
                }
            }

            approval_event = approval_event_rx.recv() => {
                let Some(event) = approval_event else { break };
                // Forward the runtime-produced summary without inspecting or
                // reconstructing it from the raw argument object.
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

/// Returns how many messages were appended, so the caller can advance its
/// persisted-transcript watermark by exactly what landed on disk.
fn persist_conversation_messages(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    session_key: &str,
    messages: &[zeroclaw_providers::ConversationMessage],
) -> usize {
    // if the user deleted the session between the turn starting and
    // the post-turn persistence, don't resurrect it. The `aborted` / `done`
    // / `error` frames are still sent to the client; we just refuse to
    // re-create the row that `DELETE /api/sessions/{id}` just wiped.
    if !backend.session_exists(session_key) {
        return 0;
    }
    let mut appended = 0;
    for message in messages {
        let zeroclaw_providers::ConversationMessage::Chat(message) = message else {
            continue;
        };
        if message.role == "system" {
            continue;
        }
        if backend.append(session_key, message).is_ok() {
            appended += 1;
        }
    }
    appended
}

/// Rebuild a connection's execution history from the persisted transcript
/// when another writer advanced it since this connection last incorporated
/// it — typically the detached turn a reconnecting socket queued behind. Must
/// run while holding the session permit, so the transcript cannot move again
/// before the turn that follows. Clears before re-seeding so nothing is
/// duplicated; returns the trim notice re-seeding may produce.
fn refresh_history_if_advanced(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    agent: &mut zeroclaw_runtime::agent::Agent,
    session_key: &str,
    persisted_watermark: &mut usize,
) -> Option<zeroclaw_api::agent::TurnEvent> {
    let persisted = backend.load(session_key);
    if persisted.len() == *persisted_watermark {
        return None;
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "session_key": session_key,
                "known_messages": *persisted_watermark,
                "persisted_messages": persisted.len(),
            })
        ),
        "session transcript advanced since this connection loaded it; rebuilding execution history"
    );
    *persisted_watermark = persisted.len();
    agent.clear_history();
    if persisted.is_empty() {
        return None;
    }
    agent.seed_history_with_event(&persisted)
}

/// One frame from the client socket, as seen by the mid-turn forward loop.
enum ClientFrame {
    Text(axum::extract::ws::Utf8Bytes),
    Ping(axum::body::Bytes),
    /// Pong or binary: nothing to do.
    Ignore,
    /// Close frame, transport error, or end of stream: the viewer is gone.
    /// This does not cancel the turn; see `process_chat_message`.
    Gone,
}

fn classify_client_frame(frame: Option<Result<Message, axum::Error>>) -> ClientFrame {
    match frame {
        Some(Ok(Message::Text(text))) => ClientFrame::Text(text),
        Some(Ok(Message::Ping(payload))) => ClientFrame::Ping(payload),
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => ClientFrame::Gone,
        Some(Ok(Message::Pong(_) | Message::Binary(_))) => ClientFrame::Ignore,
    }
}

/// The viewer went away mid-turn. Log it and drop every parked approval
/// oneshot: with nobody attached to answer, the approval channel resolves each
/// as an unreachable-operator deny immediately instead of after the prompt
/// timeout. The turn itself keeps running.
fn detach_mid_turn(session_key: &str, turn_id: &str, pending_approvals: &PendingApprovals) {
    let parked: Vec<_> = pending_approvals.lock().drain().collect();
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "session_key": session_key,
                "trace_id": turn_id,
                "parked_approvals": parked.len(),
            })
        ),
        "WebSocket client detached mid-turn; turn continues unattended"
    );
    drop(parked);
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
) -> serde_json::Value {
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

fn first_chat_message_content(text: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(text).ok()?;
    (parsed["type"].as_str() == Some("message"))
        .then(|| parsed["content"].as_str().unwrap_or("").to_string())
        .filter(|content| !content.is_empty())
}

fn event_matches_session(event: &serde_json::Value, session_id: &str) -> bool {
    match event.get("session_id").and_then(|value| value.as_str()) {
        Some(event_session_id) => event_session_id == session_id,
        None => is_global_chat_event(event),
    }
}

fn is_global_chat_event(event: &serde_json::Value) -> bool {
    matches!(
        event.get("type").and_then(serde_json::Value::as_str),
        Some("cron_result")
    )
}

fn is_observability_telemetry(event: &serde_json::Value) -> bool {
    event.get("source").and_then(serde_json::Value::as_str) == Some("observability")
}

/// Process a single chat message through the agent and send the response.
/// Uses [`Agent::turn_streamed`] so that intermediate text chunks, tool calls,
/// and tool results are forwarded to the WebSocket client in real time.
///
/// Returns `true` when the client went away mid-turn. The turn still ran to
/// completion and was persisted; the caller should stop servicing the socket.
#[allow(clippy::too_many_arguments)]
async fn process_chat_message(
    state: &AppState,
    agent: &mut zeroclaw_runtime::agent::Agent,
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    receiver: &mut futures_util::stream::SplitStream<WebSocket>,
    approval_event_rx: &mut tokio::sync::mpsc::Receiver<zeroclaw_api::agent::TurnEvent>,
    pending_approvals: &PendingApprovals,
    ping_interval: &mut Option<tokio::time::Interval>,
    ws_memory: &Option<Arc<dyn zeroclaw_memory::Memory>>,
    // Persisted messages this connection's history already reflects; advanced
    // by exactly what this turn persists.
    persisted_watermark: &mut usize,
    content: &str,
    session_key: &str,
    session_id: &str,
    // Transport-authenticated approval subject (paired-token hash), threaded so a
    // mid-turn SOP approval frame carries the same identity as the top-level path.
    auth_subject: Option<&str>,
) -> bool {
    use futures_util::StreamExt as _;
    use zeroclaw_runtime::agent::TurnEvent;

    // The caller holds the session permit. A socket that connected while a
    // detached turn was still running seeded its history from a transcript
    // that turn has since extended; rebuild from the persisted transcript
    // before running on the stale snapshot.
    if let Some(ref backend) = state.session_backend
        && let Some(TurnEvent::HistoryTrimmed {
            dropped_messages,
            kept_turns,
            reason,
        }) =
            refresh_history_if_advanced(backend.as_ref(), agent, session_key, persisted_watermark)
    {
        let frame = history_trimmed_ws_frame(dropped_messages, kept_turns, &reason);
        let _ = sender.send(Message::Text(frame.to_string().into())).await;
    }

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

    let content_owned = content.to_string();
    let session_key_owned = session_key.to_string();
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
                    agent
                        .turn_streamed_with_steering_state(
                            &content_owned,
                            event_tx,
                            Some(cancel_token.clone()),
                            Some(&mut steering_rx),
                        )
                        .instrument(span),
                ),
            ),
        )
        .await
    };

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
    // The socket is a viewer of the turn, not its owner. Once the client is
    // gone — close frame, transport error, end of stream, or a failed write —
    // this loop stops touching the socket but keeps draining `event_rx` so the
    // agent runs to completion and persists its real response. Only the
    // explicit abort path (`cancel_token`, reached through
    // `POST /api/sessions/{id}/abort`) cancels a turn. The socket arms are
    // *disabled* once the client is gone rather than `continue`d: re-polling a
    // closed receiver hot-loops the select and starves the abort endpoint.
    let forward_fut = async {
        let mut cancel_drained = false;
        let mut client_gone = false;
        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled(), if !cancel_drained => {
                    let drained: Vec<_> = pending_approvals.lock().drain().collect();
                    drop(drained);
                    cancel_drained = true;
                    // Fall through; the agent loop will now wake from the
                    // approval await, see the cancel token, and propagate
                    // a ToolLoopCancelled error which closes event_rx and
                    // breaks this loop on the `event_rx.recv()` arm below.
                }
                client_msg = receiver.next(), if !client_gone => {
                    let text = match classify_client_frame(client_msg) {
                        ClientFrame::Text(text) => text,
                        ClientFrame::Ping(payload) => {
                            if sender.send(Message::Pong(payload)).await.is_err() {
                                detach_mid_turn(session_key, &turn_id, pending_approvals);
                                client_gone = true;
                            }
                            continue;
                        }
                        ClientFrame::Ignore => continue,
                        ClientFrame::Gone => {
                            detach_mid_turn(session_key, &turn_id, pending_approvals);
                            client_gone = true;
                            continue;
                        }
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
                            // A SOP-kind frame is a gate resolution (keyed by run_id),
                            // not a tool-prompt response (keyed by request_id). Resolve
                            // it here too so it is answered mid-turn instead of being
                            // silently dropped on the request_id path below.
                            if handle_ws_sop_frame(
                                &parsed,
                                state,
                                session_id,
                                auth_subject,
                                &mut *sender,
                            )
                            .await
                            {
                                continue;
                            }
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
                        if client_gone {
                            // Nobody is attached to answer: drop the parked
                            // oneshot so the approval channel resolves it as an
                            // unreachable-operator deny now, not at the timeout.
                            drop(pending_approvals.lock().remove(&request_id));
                            continue;
                        }
                        let frame = serde_json::json!({
                            "type": "approval_request",
                            "request_id": request_id,
                            "tool": tool_name,
                            "arguments_summary": arguments_summary,
                            "timeout_secs": timeout_secs,
                        });
                        if sender.send(Message::Text(frame.to_string().into())).await.is_err() {
                            detach_mid_turn(session_key, &turn_id, pending_approvals);
                            client_gone = true;
                        }
                    }
                }
                _ = tick_websocket_ping(ping_interval), if !client_gone => {
                    if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                        detach_mid_turn(session_key, &turn_id, pending_approvals);
                        client_gone = true;
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
                            serde_json::json!({ "type": "chunk", "content": delta })
                        }
                        TurnEvent::Thinking { delta } => {
                            serde_json::json!({ "type": "thinking", "content": delta })
                        }
                        TurnEvent::ToolCall { id, name, args } => {
                            serde_json::json!({ "type": "tool_call", "id": id, "name": name, "args": args })
                        }
                        TurnEvent::ToolResult {
                            id, name, output, ..
                        } => {
                            serde_json::json!({ "type": "tool_result", "id": id, "name": name, "output": output })
                        }
                        TurnEvent::ApprovalRequest {
                            request_id,
                            tool_name,
                            arguments_summary,
                            timeout_secs,
                        } => {
                            if client_gone {
                                drop(pending_approvals.lock().remove(&request_id));
                                continue;
                            }
                            serde_json::json!({
                                "type": "approval_request",
                                "request_id": request_id,
                                "tool": tool_name,
                                "arguments_summary": arguments_summary,
                                "timeout_secs": timeout_secs,
                            })
                        }
                        TurnEvent::HistoryTrimmed {
                            dropped_messages,
                            kept_turns,
                            reason,
                        } => history_trimmed_ws_frame(dropped_messages, kept_turns, &reason),
                        TurnEvent::Plan { entries } => serde_json::json!({
                            "type": "plan",
                            "entries": entries,
                        }),
                    };
                    if client_gone {
                        continue;
                    }
                    if sender.send(Message::Text(ws_msg.to_string().into())).await.is_err() {
                        detach_mid_turn(session_key, &turn_id, pending_approvals);
                        client_gone = true;
                    }
                }
            }
        }
        client_gone
    };

    let (result, client_gone) = tokio::join!(turn_fut, forward_fut);

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
            let still_exists = backend.session_exists(session_key);
            if still_exists {
                match &result {
                    Err(error) if !error.new_messages.is_empty() => {
                        *persisted_watermark += persist_conversation_messages(
                            backend.as_ref(),
                            session_key,
                            &error.new_messages,
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
                                if backend.append(session_key, &assistant_msg).is_ok() {
                                    *persisted_watermark += 1;
                                }
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
                            if backend.append(session_key, &assistant_msg).is_ok() {
                                *persisted_watermark += 1;
                            }
                        }
                    }
                }
            }
        }

        // Inform the client the turn was aborted
        let aborted = serde_json::json!({ "type": "aborted" });
        let _ = sender.send(Message::Text(aborted.to_string().into())).await;

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

        return client_gone;
    }

    match result {
        Ok(outcome) => {
            if let Some(ref backend) = state.session_backend {
                *persisted_watermark += persist_conversation_messages(
                    backend.as_ref(),
                    session_key,
                    &outcome.new_messages,
                );
            }

            // Fire-and-forget memory consolidation so facts from WS sessions
            // are extracted to long-term memory (Daily + Core categories).
            if state.auto_save {
                if let Some(mem) = ws_memory.clone() {
                    let model_provider = state.model_provider.clone();
                    let model = state.model.clone();
                    let temperature = state.temperature;
                    let memory_config = state.config.read().memory.clone();
                    let user_msg = content.to_string();
                    let assistant_resp = outcome.response.clone();
                    zeroclaw_spawn::spawn!(async move {
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

            let done = serde_json::json!({
                "type": "done",
                "full_response": outcome.response,
                "input_tokens": total_input_tokens,
                "output_tokens": total_output_tokens,
                "tokens_used": total_tokens,
                "cost_usd": cost_usd,
                "model": turn_model,
                "provider": provider_label,
                "max_context_tokens": max_context_tokens,
                "last_input_tokens": last_input_tokens,
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
                "model": turn_model,
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
                        "model": turn_model,
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
                *persisted_watermark +=
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
            let user_message =
                zeroclaw_runtime::agent::terminal_completion_error_message(&e.error, None);
            let err = send_ws_turn_failure(sender, &e.error, user_message.as_deref()).await;

            // Broadcast error event
            let _ = state.event_tx.send(serde_json::json!({
                "type": "error",
                "component": "ws_chat",
                "message": err["message"],
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
                        "error": zeroclaw_providers::sanitize_api_error(&e.error.to_string()),
                        "error_code": err["code"],
                        "trace_id": turn_id,
                    })),
                "gateway_ws_turn"
            );
        }
    }

    client_gone
}

/// Serialize a failed turn for the WebSocket boundary without letting a
/// localized user message alter the stable diagnostic used for classification.
fn ws_turn_failure_frame(
    diagnostic: &str,
    user_message: Option<&str>,
    is_terminal_provider_failure: bool,
) -> serde_json::Value {
    let sanitized = zeroclaw_providers::sanitize_api_error(diagnostic);
    let error_code = if is_terminal_provider_failure {
        "PROVIDER_ERROR"
    } else if sanitized.to_lowercase().contains("api key")
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
    serde_json::json!({
        "type": "error",
        "message": user_message.unwrap_or(&sanitized),
        "code": error_code,
    })
}

async fn send_ws_turn_failure<S>(
    sender: &mut S,
    error: &anyhow::Error,
    user_message: Option<&str>,
) -> serde_json::Value
where
    S: futures_util::Sink<Message> + Unpin,
{
    let frame = ws_turn_failure_frame(&error.to_string(), user_message, user_message.is_some());
    let _ = sender.send(Message::Text(frame.to_string().into())).await;
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, header},
        routing::{get, post},
    };
    use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};

    #[test]
    fn ws_terminal_failure_uses_localized_message_without_reclassifying_diagnostic() {
        let diagnostic = "provider completed without final text or tool calls";
        let localized = "Réponse terminale invalide.";

        let frame = ws_turn_failure_frame(diagnostic, Some(localized), true);

        assert_eq!(frame["type"], "error");
        assert_eq!(frame["message"], "Réponse terminale invalide.");
        assert_eq!(frame["code"], "PROVIDER_ERROR");
        assert!(
            !frame["message"]
                .as_str()
                .unwrap_or_default()
                .contains(diagnostic),
            "WebSocket delivery must not fall back to the diagnostic when Fluent supplies text"
        );
    }

    #[test]
    fn websocket_handler_projects_anthropic_empty_terminal_stream_as_user_error() {
        // This production-shaped fixture exceeds the Linux test harness's
        // default stack; isolate only this test instead of weakening CI-wide
        // stack limits or dropping the real WebSocket boundary coverage.
        std::thread::Builder::new()
            .name("ws-empty-terminal-regression".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime")
                    .block_on(websocket_handler_projects_anthropic_empty_terminal_stream_as_user_error_inner());
            })
            .expect("spawn WebSocket regression thread")
            .join()
            .expect("WebSocket regression thread must not panic");
    }

    async fn websocket_handler_projects_anthropic_empty_terminal_stream_as_user_error_inner() {
        // This is a real WebSocket upgrade and a real agent built from live
        // config. The local Anthropic-shaped server completes an empty SSE
        // response, then returns an empty non-stream fallback, exercising the
        // production path through `process_chat_message` to the client.
        let mock_app = Router::new().route(
            "/v1/messages",
            post(|Json(request): Json<serde_json::Value>| async move {
                if request["stream"].as_bool() == Some(true) {
                    (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1}}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\n",
                    )
                        .into_response()
                } else {
                    Json(serde_json::json!({
                        "id": "msg_test",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-test",
                        "content": [],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 1, "output_tokens": 1}
                    }))
                    .into_response()
                }
            }),
        );
        let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local Anthropic fixture");
        let mock_addr = mock_listener.local_addr().expect("fixture address");
        let mock_server = zeroclaw_spawn::spawn!(async move {
            axum::serve(mock_listener, mock_app)
                .await
                .expect("local Anthropic fixture serves");
        });

        let tmp = tempfile::tempdir().expect("temporary gateway workspace");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("gateway workspace");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: workspace.clone(),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.memory.backend = "none".to_string();
        config.reliability.provider_retries = 0;
        config.providers.models.anthropic.insert(
            "fixture".to_string(),
            zeroclaw_config::schema::AnthropicModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    api_key: Some("test-key".to_string()),
                    uri: Some(format!("http://{mock_addr}")),
                    model: Some("claude-test".to_string()),
                    ..Default::default()
                },
            },
        );
        config.risk_profiles.insert(
            "fixture".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.runtime_profiles.insert(
            "fixture".to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig::default(),
        );
        config.agents.insert(
            "web".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "anthropic.fixture".into(),
                risk_profile: "fixture".into(),
                runtime_profile: "fixture".into(),
                workspace: zeroclaw_config::multi_agent::AgentWorkspaceConfig {
                    path: Some(workspace),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let state = crate::api::tests::test_state(config);
        let gateway_app = Router::new()
            .route("/ws/chat", get(handle_ws_chat))
            .with_state(state);
        let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local WebSocket gateway");
        let gateway_addr = gateway_listener.local_addr().expect("gateway address");
        let gateway_server = zeroclaw_spawn::spawn!(async move {
            axum::serve(gateway_listener, gateway_app)
                .await
                .expect("local WebSocket gateway serves");
        });

        let (mut client, _) = connect_async(format!("ws://{gateway_addr}/ws/chat?agent=web"))
            .await
            .expect("WebSocket upgrade");
        let first = client
            .next()
            .await
            .expect("session_start frame")
            .expect("session_start");
        assert!(
            first
                .into_text()
                .expect("text session_start")
                .contains("session_start")
        );
        client
            .send(ClientMessage::Text(r#"{"type":"connect"}"#.into()))
            .await
            .expect("connect frame");
        let connected = client
            .next()
            .await
            .expect("connected frame")
            .expect("connected");
        assert!(
            connected
                .into_text()
                .expect("text connected")
                .contains("connected")
        );
        client
            .send(ClientMessage::Text(
                r#"{"type":"message","content":"test"}"#.into(),
            ))
            .await
            .expect("chat message");

        let mut terminal_error = None;
        for _ in 0..8 {
            let frame = tokio::time::timeout(Duration::from_secs(3), client.next())
                .await
                .expect("gateway response deadline")
                .expect("gateway stays connected")
                .expect("gateway frame");
            let text = frame.into_text().expect("text gateway frame");
            let json: serde_json::Value = serde_json::from_str(&text).expect("JSON gateway frame");
            if json["type"] == "error" {
                terminal_error = Some(json);
                break;
            }
        }

        let error = terminal_error.expect("empty terminal response reaches WebSocket client");
        assert_eq!(error["code"], "PROVIDER_ERROR");
        assert_eq!(
            error["message"],
            zeroclaw_runtime::agent::semantic_empty_terminal_completion_message(None),
        );
        assert_ne!(
            error["message"], "provider completed without final text or tool calls",
            "stable diagnostic must not leak into the user-facing WebSocket frame"
        );

        gateway_server.abort();
        mock_server.abort();
    }

    /// Production-shaped WebSocket fixtures exceed the Linux test harness's
    /// default thread stack; run them on a dedicated 8 MiB thread with a
    /// current-thread runtime instead of weakening CI-wide stack limits.
    fn run_ws_regression<Fut>(name: &'static str, test: impl FnOnce() -> Fut + Send + 'static)
    where
        Fut: std::future::Future<Output = ()>,
    {
        std::thread::Builder::new()
            .name(name.to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime")
                    .block_on(test());
            })
            .expect("spawn WebSocket regression thread")
            .join()
            .expect("WebSocket regression thread must not panic");
    }

    /// Text the parked provider fixture finishes with once released.
    const PARKED_TURN_RESPONSE: &str = "finished while nobody was watching";

    /// Anthropic-shaped SSE prefix. The fixture sends this immediately so the
    /// gateway turn is provably in flight, then parks until released.
    const PARKED_STREAM_HEAD: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1}}}\n\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n";

    fn parked_stream_tail() -> String {
        format!(
            "event: content_block_delta\n\
data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{PARKED_TURN_RESPONSE}\"}}}}\n\n\
event: content_block_stop\n\
data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
event: message_delta\n\
data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":1}}}}\n\n\
event: message_stop\n\
data: {{\"type\":\"message_stop\"}}\n\n"
        )
    }

    type WsClient = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// A real gateway whose `web` agent talks to a local Anthropic-shaped
    /// fixture that parks every streaming completion until the test releases
    /// it, so a turn can be held in flight while the client goes away.
    struct ParkedTurnFixture {
        state: AppState,
        backend: Arc<dyn zeroclaw_infra::session_backend::SessionBackend>,
        gateway_addr: std::net::SocketAddr,
        /// The body of every streaming request the provider fixture receives,
        /// in arrival order.
        request_seen: tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
        /// How many parked completions may finish; the n-th request finishes
        /// once this reaches n.
        released: tokio::sync::watch::Sender<usize>,
        servers: Vec<tokio::task::JoinHandle<()>>,
        _tmp: tempfile::TempDir,
    }

    impl ParkedTurnFixture {
        async fn spawn() -> Self {
            let (request_seen_tx, request_seen) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
            let (released, released_rx) = tokio::sync::watch::channel(0usize);
            let arrivals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mock_app = Router::new().route(
                "/v1/messages",
                post(move |Json(request): Json<serde_json::Value>| {
                    let request_seen_tx = request_seen_tx.clone();
                    let mut released_rx = released_rx.clone();
                    let index = arrivals.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    async move {
                        assert_eq!(
                            request["stream"].as_bool(),
                            Some(true),
                            "gateway chat turns stream from the provider"
                        );
                        let head = futures_util::stream::once(async move {
                            let _ = request_seen_tx.send(request);
                            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                                PARKED_STREAM_HEAD.as_bytes(),
                            ))
                        });
                        let tail = futures_util::stream::once(async move {
                            let _ = released_rx.wait_for(|released| *released >= index).await;
                            Ok::<_, std::io::Error>(axum::body::Bytes::from(parked_stream_tail()))
                        });
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            axum::body::Body::from_stream(head.chain(tail)),
                        )
                    }
                }),
            );
            let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind parked provider fixture");
            let mock_addr = mock_listener.local_addr().expect("fixture address");
            let mock_server = zeroclaw_spawn::spawn!(async move {
                axum::serve(mock_listener, mock_app)
                    .await
                    .expect("parked provider fixture serves");
            });

            let tmp = tempfile::tempdir().expect("temporary gateway workspace");
            let workspace = tmp.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("gateway workspace");
            let mut config = zeroclaw_config::schema::Config {
                data_dir: workspace.clone(),
                config_path: tmp.path().join("config.toml"),
                ..Default::default()
            };
            config.memory.backend = "none".to_string();
            config.reliability.provider_retries = 0;
            config.providers.models.anthropic.insert(
                "fixture".to_string(),
                zeroclaw_config::schema::AnthropicModelProviderConfig {
                    base: zeroclaw_config::schema::ModelProviderConfig {
                        api_key: Some("test-key".to_string()),
                        uri: Some(format!("http://{mock_addr}")),
                        model: Some("claude-test".to_string()),
                        ..Default::default()
                    },
                },
            );
            config.risk_profiles.insert(
                "fixture".to_string(),
                zeroclaw_config::schema::RiskProfileConfig::default(),
            );
            config.runtime_profiles.insert(
                "fixture".to_string(),
                zeroclaw_config::schema::RuntimeProfileConfig::default(),
            );
            config.agents.insert(
                "web".to_string(),
                zeroclaw_config::schema::AliasedAgentConfig {
                    model_provider: "anthropic.fixture".into(),
                    risk_profile: "fixture".into(),
                    runtime_profile: "fixture".into(),
                    workspace: zeroclaw_config::multi_agent::AgentWorkspaceConfig {
                        path: Some(workspace.clone()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );

            let backend = zeroclaw_infra::make_session_backend(&workspace, "sqlite")
                .expect("sqlite session backend");
            let state = AppState {
                session_backend: Some(backend.clone()),
                ..crate::api::tests::test_state(config)
            };
            let gateway_app = Router::new()
                .route("/ws/chat", get(handle_ws_chat))
                .with_state(state.clone());
            let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local WebSocket gateway");
            let gateway_addr = gateway_listener.local_addr().expect("gateway address");
            let gateway_server = zeroclaw_spawn::spawn!(async move {
                axum::serve(gateway_listener, gateway_app)
                    .await
                    .expect("local WebSocket gateway serves");
            });

            Self {
                state,
                backend,
                gateway_addr,
                request_seen,
                released,
                servers: vec![mock_server, gateway_server],
                _tmp: tmp,
            }
        }

        /// Open a chat socket on `session_id` and return it with the
        /// `session_start` frame the gateway greets it with.
        async fn connect(&self, session_id: &str) -> (WsClient, serde_json::Value) {
            let (mut client, _) = connect_async(format!(
                // This URL connects only to the test's loopback listener.
                "ws://{}/ws/chat?agent=web&session_id={session_id}", // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
                self.gateway_addr
            ))
            .await
            .expect("WebSocket upgrade");
            let session_start = next_text_frame(&mut client).await;
            assert_eq!(session_start["type"], "session_start");
            (client, session_start)
        }

        /// Send a chat message and block until the provider fixture holds the
        /// turn's streaming request, i.e. the turn is provably in flight.
        /// Returns that request body.
        async fn start_parked_turn(
            &mut self,
            client: &mut WsClient,
            content: &str,
        ) -> serde_json::Value {
            send_chat(client, content).await;
            self.next_provider_request().await
        }

        /// The next streaming request the provider fixture receives.
        async fn next_provider_request(&mut self) -> serde_json::Value {
            tokio::time::timeout(Duration::from_secs(10), self.request_seen.recv())
                .await
                .expect("provider request deadline")
                .expect("provider fixture observes the turn's request")
        }

        /// A live turn holds its abort token for the session for its whole
        /// duration; that is what `POST /api/sessions/{id}/abort` cancels.
        fn has_live_turn(&self, session_key: &str) -> bool {
            self.state
                .cancel_tokens
                .lock()
                .expect("cancel_tokens lock")
                .contains_key(session_key)
        }

        fn session_state(&self, session_key: &str) -> Option<String> {
            self.backend
                .get_session_state(session_key)
                .expect("session state")
                .map(|state| state.state)
        }

        /// Let the next parked completion finish with `PARKED_TURN_RESPONSE`.
        fn release_next(&self) {
            self.released.send_modify(|released| *released += 1);
        }

        /// Wait until the gateway has finished the turn: the abort token is
        /// gone and the persisted session state has settled back to `idle`.
        async fn wait_for_turn_to_settle(&self, session_key: &str) {
            tokio::time::timeout(Duration::from_secs(10), async {
                while self.has_live_turn(session_key)
                    || self.session_state(session_key).as_deref() != Some("idle")
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("turn settles within the deadline");
        }

        fn shutdown(self) {
            for server in self.servers {
                server.abort();
            }
        }
    }

    async fn send_chat(client: &mut WsClient, content: &str) {
        client
            .send(ClientMessage::Text(
                serde_json::json!({"type": "message", "content": content})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("chat message");
    }

    /// `(role, text)` per message of an Anthropic-shaped request body, with
    /// block-array content flattened to its concatenated text.
    fn provider_messages(request: &serde_json::Value) -> Vec<(String, String)> {
        request["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|message| {
                let role = message["role"].as_str().unwrap_or_default().to_string();
                let text = match &message["content"] {
                    serde_json::Value::String(text) => text.clone(),
                    serde_json::Value::Array(blocks) => blocks
                        .iter()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join(""),
                    _ => String::new(),
                };
                (role, text)
            })
            .collect()
    }

    async fn next_text_frame(client: &mut WsClient) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = client
                    .next()
                    .await
                    .expect("gateway frame")
                    .expect("gateway transport");
                if let ClientMessage::Text(text) = frame {
                    break serde_json::from_str::<serde_json::Value>(&text)
                        .expect("JSON gateway frame");
                }
            }
        })
        .await
        .expect("gateway frame deadline")
    }

    /// Detach the viewer mid-turn: close handshake, drop the socket, then give
    /// the gateway a moment to observe it. Under the old contract this is the
    /// point where the turn was cancelled — its abort token vanished within
    /// milliseconds and the transcript ended with the interruption marker.
    async fn disconnect_viewer(client: WsClient) {
        let mut client = client;
        client.close(None).await.expect("client close frame");
        drop(client);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    #[test]
    fn websocket_client_disconnect_mid_turn_lets_the_turn_finish_and_persist() {
        run_ws_regression(
            "ws-detach-survives",
            websocket_client_disconnect_mid_turn_lets_the_turn_finish_and_persist_inner,
        );
    }

    async fn websocket_client_disconnect_mid_turn_lets_the_turn_finish_and_persist_inner() {
        // Regression: navigating away, closing the tab, or a dropped
        // connection must not cancel a running agent turn. The turn
        // finishes unattended, persists its real response, and a later socket
        // on the same session resumes the completed transcript.
        let mut fixture = ParkedTurnFixture::spawn().await;
        let session_id = "detach-regression";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");
        let (mut client, session_start) = fixture.connect(session_id).await;
        assert_eq!(session_start["resumed"], false);

        let prompt = "keep working while I am away";
        fixture.start_parked_turn(&mut client, prompt).await;
        assert!(
            fixture.has_live_turn(&session_key),
            "an in-flight turn registers its abort token"
        );

        disconnect_viewer(client).await;
        assert!(
            fixture.has_live_turn(&session_key),
            "a client disconnect must not cancel the running turn"
        );
        assert_eq!(
            fixture.session_state(&session_key).as_deref(),
            Some("running"),
            "the detached turn is still reported as running"
        );

        // Nobody is attached; let the provider finish the turn.
        fixture.release_next();
        fixture.wait_for_turn_to_settle(&session_key).await;

        let transcript = fixture.backend.load(&session_key);
        let turns: Vec<(&str, &str)> = transcript
            .iter()
            .map(|message| (message.role.as_str(), message.content.as_str()))
            .collect();
        // The runtime prefixes the persisted user turn with its date stamp;
        // the prompt itself is what must survive.
        assert!(
            matches!(turns.as_slice(), [("user", user), ("assistant", PARKED_TURN_RESPONSE)] if user.ends_with(prompt)),
            "the detached turn persists its real response, not an interruption marker: {turns:?}"
        );
        let interrupted =
            zeroclaw_runtime::i18n::get_required_cli_string("turn-interrupted-by-user");
        assert!(
            !transcript
                .iter()
                .any(|message| message.content.contains(&interrupted)),
            "no message carries the interruption marker: {turns:?}"
        );

        // Reattach: a new socket on the same session resumes the finished turn.
        let (client, session_start) = fixture.connect(session_id).await;
        assert_eq!(session_start["resumed"], true);
        assert_eq!(session_start["message_count"], transcript.len());
        drop(client);
        fixture.shutdown();
    }

    #[test]
    fn reconnect_during_detached_turn_refreshes_history_before_the_follow_up() {
        run_ws_regression(
            "ws-detach-reconnect-history",
            reconnect_during_detached_turn_refreshes_history_before_the_follow_up_inner,
        );
    }

    async fn reconnect_during_detached_turn_refreshes_history_before_the_follow_up_inner() {
        // A socket that reconnects while the detached turn is still running
        // seeds its agent from the transcript as persisted at that moment. Its
        // follow-up waits for that turn on the session permit and must then
        // run on the transcript the turn persisted, not on the stale snapshot.
        let mut fixture = ParkedTurnFixture::spawn().await;
        let session_id = "detach-reconnect";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");
        let first_prompt = "start the long task";
        let follow_up = "now continue from where that left off";

        let (mut first, _) = fixture.connect(session_id).await;
        let first_request = fixture.start_parked_turn(&mut first, first_prompt).await;
        let first_messages = provider_messages(&first_request);
        assert!(
            matches!(first_messages.last(), Some((role, text)) if role == "user" && text.ends_with(first_prompt)),
            "the first turn carries its own prompt: {first_messages:?}"
        );
        disconnect_viewer(first).await;
        assert!(fixture.has_live_turn(&session_key));

        // Reconnect before the detached turn finishes: nothing is persisted
        // yet, so this socket's history snapshot is empty.
        let (mut second, session_start) = fixture.connect(session_id).await;
        assert_eq!(session_start["resumed"], false);
        assert_eq!(session_start["message_count"], 0);

        // The follow-up queues behind the running turn on the session permit;
        // it must not start a concurrent turn.
        send_chat(&mut second, follow_up).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            fixture.has_live_turn(&session_key),
            "the detached turn is still the live turn"
        );
        assert!(
            fixture.request_seen.try_recv().is_err(),
            "the follow-up waits for the permit instead of reaching the provider"
        );

        // Let the first turn finish. The queued follow-up then runs, and its
        // provider request must include what the first turn persisted.
        fixture.release_next();
        let second_request = fixture.next_provider_request().await;
        let messages = provider_messages(&second_request);
        let tail: Vec<(&str, &str)> = messages
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|(role, text)| (role.as_str(), text.as_str()))
            .collect();
        assert!(
            matches!(
                tail.as_slice(),
                [("user", earlier), ("assistant", PARKED_TURN_RESPONSE), ("user", latest)]
                    if earlier.ends_with(first_prompt) && latest.ends_with(follow_up)
            ),
            "the follow-up runs on the transcript the detached turn persisted: {messages:?}"
        );

        // The follow-up completes on the attached socket with its own response.
        fixture.release_next();
        let done = loop {
            let frame = next_text_frame(&mut second).await;
            match frame["type"].as_str() {
                Some("done") => break frame,
                Some("error") => panic!("follow-up turn failed: {frame}"),
                _ => {}
            }
        };
        assert_eq!(done["full_response"], PARKED_TURN_RESPONSE);
        fixture.wait_for_turn_to_settle(&session_key).await;

        let transcript = fixture.backend.load(&session_key);
        let turns: Vec<(&str, &str)> = transcript
            .iter()
            .map(|message| (message.role.as_str(), message.content.as_str()))
            .collect();
        assert!(
            matches!(
                turns.as_slice(),
                [
                    ("user", earlier),
                    ("assistant", PARKED_TURN_RESPONSE),
                    ("user", latest),
                    ("assistant", PARKED_TURN_RESPONSE),
                ] if earlier.ends_with(first_prompt) && latest.ends_with(follow_up)
            ),
            "both turns persist in order without duplication: {turns:?}"
        );
        drop(second);
        fixture.shutdown();
    }

    #[test]
    fn session_abort_still_cancels_a_detached_turn() {
        run_ws_regression(
            "ws-detach-abort",
            session_abort_still_cancels_a_detached_turn_inner,
        );
    }

    async fn session_abort_still_cancels_a_detached_turn_inner() {
        // The explicit abort endpoint remains the one thing that cancels a
        // turn, and it keeps working after the viewer has gone: the detached
        // turn neither hot-loops nor outlives an abort.
        let mut fixture = ParkedTurnFixture::spawn().await;
        let session_id = "detach-then-abort";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");
        let (mut client, _) = fixture.connect(session_id).await;
        fixture
            .start_parked_turn(&mut client, "keep working until I say stop")
            .await;

        disconnect_viewer(client).await;
        assert!(fixture.has_live_turn(&session_key));

        let response = crate::api::handle_api_session_abort(
            axum::extract::State(fixture.state.clone()),
            HeaderMap::new(),
            axum::extract::Path(session_id.to_string()),
        )
        .await
        .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        fixture.wait_for_turn_to_settle(&session_key).await;

        let transcript = fixture.backend.load(&session_key);
        let interrupted =
            zeroclaw_runtime::i18n::get_required_cli_string("turn-interrupted-by-user");
        assert!(
            transcript.iter().any(
                |message| message.role == "assistant" && message.content.contains(&interrupted)
            ),
            "an explicit abort persists the interruption marker: {transcript:?}"
        );
        assert!(
            !transcript
                .iter()
                .any(|message| message.content.contains(PARKED_TURN_RESPONSE)),
            "the aborted turn never received the provider's response"
        );
        fixture.release_next();
        fixture.shutdown();
    }

    #[tokio::test]
    async fn websocket_ping_interval_skips_missed_ticks() {
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.websocket_ping_interval_secs = 1;

        let interval = websocket_ping_interval(&config).expect("enabled ping interval");

        assert_eq!(
            interval.missed_tick_behavior(),
            tokio::time::MissedTickBehavior::Skip
        );
    }

    #[test]
    fn websocket_ping_interval_handles_unvalidated_overflow_without_panicking() {
        let mut config = zeroclaw_config::schema::Config::default();
        config.gateway.websocket_ping_interval_secs = u64::MAX;

        assert!(websocket_ping_interval(&config).is_none());
    }

    #[tokio::test]
    async fn idle_chat_route_pings_before_and_preserves_the_first_client_message() {
        use axum::{Router, routing::get};
        use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};
        use zeroclaw_config::{
            multi_agent::MemoryBackendKind,
            schema::{AliasedAgentConfig, Config},
        };

        let tmp = tempfile::TempDir::new().expect("temporary config root");
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).expect("test data directory");
        config.gateway.websocket_ping_interval_secs = 1;
        let mut agent = AliasedAgentConfig::default();
        agent.memory.backend = MemoryBackendKind::None;
        config.agents.insert("web".to_string(), agent);

        let app = Router::new()
            .route("/ws/chat", get(handle_ws_chat))
            .with_state(crate::api::test_state(config));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test listener");
        let address = listener.local_addr().expect("test listener address");
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("test gateway server");
        });

        let (mut socket, _) = connect_async(format!(
            // This URL connects only to the test's loopback listener.
            "ws://{address}/ws/chat?agent=web&session_id=idle-test" // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
        ))
        .await
        .expect("chat WebSocket upgrade");

        let session_start = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("session_start timeout")
            .expect("session_start frame")
            .expect("session_start transport");
        assert!(matches!(session_start, ClientMessage::Text(_)));

        let ping = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("idle ping timeout")
            .expect("idle ping frame")
            .expect("idle ping transport");
        assert!(matches!(ping, ClientMessage::Ping(_)));

        socket
            .send(ClientMessage::Text(
                serde_json::json!({"type": "message", "content": "hello"})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("first chat message after idle ping");

        let response = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let frame = socket
                    .next()
                    .await
                    .expect("response frame")
                    .expect("response transport");
                if let ClientMessage::Text(text) = frame {
                    break serde_json::from_str::<serde_json::Value>(&text)
                        .expect("JSON response frame");
                }
            }
        })
        .await
        .expect("first chat response timeout");

        assert_eq!(response["code"], "NEEDS_ONBOARDING");
        server.abort();
    }

    #[test]
    fn first_chat_message_content_preserves_the_message_for_dispatch() {
        let text = serde_json::json!({
            "type": "message",
            "content": "hello after an idle keepalive"
        })
        .to_string();

        assert_eq!(
            first_chat_message_content(&text).as_deref(),
            Some("hello after an idle keepalive")
        );
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
        let pending = new_pending_approvals();
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
        let frame = history_trimmed_ws_frame(12, 3, "message limit");

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
    fn sop_ws_error_frames_resolve_via_fluent() {
        // The SOP WebSocket error frames are UI-surfaced and route through the
        // embedded en/cli.ftl. A renamed/typo'd key would silently ship the
        // missing-key fallback `{cli-sop-ws-...}` to the browser; guard against it.
        for key in [
            "cli-sop-ws-invalid-approval",
            "cli-sop-ws-engine-lock-poisoned",
            "cli-sop-ws-subsystem-disabled",
        ] {
            let s = zeroclaw_runtime::i18n::get_required_cli_string(key);
            assert!(
                !s.starts_with('{') || !s.ends_with('}'),
                "fluent missing-key fallback leaked for {key}: {s:?}"
            );
        }
        let resolved = zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-sop-ws-resolve-failed",
            &[("error", "boom")],
        );
        assert!(
            resolved.contains("boom"),
            "the resolve-failed frame must interpolate the error: {resolved:?}"
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
        // No session_id and not on the global whitelist → dropped.
        let nameless_observability = serde_json::json!({
            "type": "agent_start",
            "source": "observability",
            "model": "gpt-4o"
        });
        // No session_id but on the global whitelist (`cron_result`) → forwarded.
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
        assert!(event_matches_session(&cron, "operator-1"));
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

    // The mid-turn `client_msg` arm in `forward_fut` must classify stream-end
    // / close / error frames as "client gone" so the arm can be *disabled*: a
    // bare `continue` re-polls the closed receiver and hot-loops the select,
    // starving the abort endpoint. A gone client must not cancel the turn;
    // that contract is proved at the route boundary by
    // `websocket_client_disconnect_mid_turn_lets_the_turn_finish_and_persist`.
    #[test]
    fn mid_turn_client_frames_classify_close_err_and_stream_end_as_gone() {
        assert!(matches!(classify_client_frame(None), ClientFrame::Gone));
        assert!(matches!(
            classify_client_frame(Some(Ok(Message::Close(None)))),
            ClientFrame::Gone
        ));
        assert!(matches!(
            classify_client_frame(Some(Err(axum::Error::new("io")))),
            ClientFrame::Gone
        ));
        assert!(matches!(
            classify_client_frame(Some(Ok(Message::Ping(Default::default())))),
            ClientFrame::Ping(_)
        ));
        assert!(matches!(
            classify_client_frame(Some(Ok(Message::Pong(Default::default())))),
            ClientFrame::Ignore
        ));
        assert!(matches!(
            classify_client_frame(Some(Ok(Message::Binary(Default::default())))),
            ClientFrame::Ignore
        ));
        assert!(matches!(
            classify_client_frame(Some(Ok(Message::Text("{}".into())))),
            ClientFrame::Text(text) if text.as_str() == "{}"
        ));
    }

    #[test]
    fn detach_mid_turn_drops_parked_approvals_so_they_fail_closed() {
        let pending = new_pending_approvals();
        let (tx, rx) = tokio::sync::oneshot::channel::<ChannelApprovalResponse>();
        pending.lock().insert("req-1".to_string(), tx);

        detach_mid_turn("gw_detached", "turn-1", &pending);

        assert!(
            pending.lock().is_empty(),
            "parked approvals are cleared on detach"
        );
        assert!(
            rx.blocking_recv().is_err(),
            "the parked oneshot is dropped, which WsApprovalChannel reports as an \
             unreachable-operator deny instead of waiting for the prompt timeout"
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

        persist_conversation_messages(&backend, "gw_deleted", &messages);

        assert!(
            backend.append_calls.lock().unwrap().is_empty(),
            "persist_conversation_messages must not resurrect a session whose \
             session_exists() returned false (see #7126)"
        );
    }

    /// A `Sink<Message>` that just collects the text frames sent to it, so a handler
    /// smoke can inspect the response without a real WebSocket.
    struct CollectSink(Vec<String>);
    impl futures_util::Sink<Message> for CollectSink {
        type Error = std::convert::Infallible;
        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn start_send(self: std::pin::Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            if let Message::Text(t) = item {
                self.get_mut().0.push(t.to_string());
            }
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn ws_sop_frame_enforces_policy_membership_via_auth_subject() {
        use zeroclaw_runtime::security::pairing::PairingGuard;
        // Reuse the HTTP policied-gate harness: a run parked at a `prod` policy whose
        // group is granted to the paired-token subject (bare, any source).
        let (state, run_id) = crate::api_sop::tests::state_with_policied_gate("ws-tok");
        let member = PairingGuard::token_hash("ws-tok");
        let outsider = PairingGuard::token_hash("someone-else");
        let frame = serde_json::json!({
            "kind": "sop",
            "run_id": run_id,
            "decision": "approve",
        });
        let run_status = |st: &AppState| {
            st.sop_engine
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .get_run(&run_id)
                .map(|r| format!("{:?}", r.status))
        };

        // A non-member WS subject is rejected; the gate stays waiting.
        let mut sink = CollectSink(Vec::new());
        assert!(
            handle_ws_sop_frame(&frame, &state, "sess-1", Some(&outsider), &mut sink).await,
            "a sop-kind frame is handled"
        );
        assert!(
            sink.0.iter().any(|m| m.contains("not_authorized")),
            "a non-member WS caller is not authorized: {:?}",
            sink.0
        );
        assert_eq!(
            run_status(&state).as_deref(),
            Some("WaitingApproval"),
            "the gate stays waiting after a non-member WS attempt"
        );

        // The member WS subject clears the policied gate.
        let mut sink = CollectSink(Vec::new());
        handle_ws_sop_frame(&frame, &state, "sess-1", Some(&member), &mut sink).await;
        assert!(
            sink.0.iter().any(|m| m.contains("resumed")),
            "an authenticated member clears the gate over WS: {:?}",
            sink.0
        );
        assert_ne!(
            run_status(&state).as_deref(),
            Some("WaitingApproval"),
            "the gate is cleared once an authorized WS member approves"
        );
    }
}
