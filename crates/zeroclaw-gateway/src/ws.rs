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
use zeroclaw_runtime::agent::TurnEvent;
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

/// Connection-scoped RAII lease over `AppState::ws_connections`: +1 on `new`,
/// -1 (removing the key at 0) on `drop`. Held for the *whole* WS connection
/// lifetime so the HTTP chat surface can 409 against cross-transport
/// concurrency. Key = raw `session_key` (`gw_<id>`), no casing folding.
struct WsConnectionGuard {
    session_key: String,
    ws_connections: Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
}

impl WsConnectionGuard {
    fn new(
        session_key: String,
        ws_connections: Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
    ) -> Self {
        ws_connections
            .lock()
            .expect("ws_connections lock poisoned")
            .entry(session_key.clone())
            .and_modify(|c| *c += 1)
            .or_insert(1);
        Self {
            session_key,
            ws_connections,
        }
    }
}

impl Drop for WsConnectionGuard {
    fn drop(&mut self) {
        let mut map = self
            .ws_connections
            .lock()
            .expect("ws_connections lock poisoned");
        if let Some(count) = map.get_mut(&self.session_key) {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.session_key);
            }
        }
    }
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

    // Session key is resolved *before* the upgrade so the connection lease
    // covers the whole WS lifetime: the guard is moved into the `on_upgrade`
    // async block and lives until `handle_socket` returns, leaving no window
    // between "upgrade accepted" and "first poll" where the lease is unheld.
    // Semantics unchanged from master (provided id or fresh UUID; `gw_<id>`).
    let session_id = params
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_key = format!("{GW_SESSION_PREFIX}{session_id}");
    let pre_ws_guard =
        WsConnectionGuard::new(session_key.clone(), Arc::clone(&state.ws_connections));

    let session_name = params.name;
    let session_cwd = params.cwd.or(params.workspace_dir);
    ws.on_upgrade(move |socket| async move {
        handle_socket(
            socket,
            state,
            agent_alias,
            session_id,
            session_key,
            pre_ws_guard,
            session_name,
            session_cwd,
            auth_subject,
        )
        .await
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
            Ok(mut g) => Some(g.resolve_via_broker(&run_id, decision, principal)),
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
    session_id: String,
    session_key: String,
    // Connection-scoped lease: held via Drop for the whole WS lifetime so the
    // HTTP chat surface can 409 against cross-transport concurrency.
    _connection_guard: WsConnectionGuard,
    session_name: Option<String>,
    session_cwd: Option<String>,
    // The transport-authenticated approval subject (paired-token hash), if the
    // connection was authenticated. Threaded to SOP approval frames so a policied
    // gate can be satisfied by an identified WS caller.
    auth_subject: Option<String>,
) {
    let (mut sender, mut receiver) = socket.split();

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
                    process_chat_message(
                        &state,
                        &mut agent,
                        &mut sender,
                        &mut receiver,
                        &mut approval_event_rx,
                        &pending_approvals,
                        &mut ping_interval,
                        &ws_memory,
                        &content,
                        &session_key,
                        &session_id,
                        auth_subject.as_deref(),
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

                process_chat_message(
                    &state,
                    &mut agent,
                    &mut sender,
                    &mut receiver,
                    &mut approval_event_rx,
                    &pending_approvals,
                    &mut ping_interval,
                    &ws_memory,
                    &content,
                    &session_key,
                    &session_id,
                        auth_subject.as_deref(),
                )
                .await;
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
/// Delegates the turn lifecycle to [`run_gateway_turn`]; the transport
/// supplies a `forward` closure that maps streamed turn events to WebSocket
/// frames, handles steering/approval/ping, and returns usage observations.
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
    content: &str,
    session_key: &str,
    session_id: &str,
    // Transport-authenticated approval subject (paired-token hash), threaded so a
    // mid-turn SOP approval frame carries the same identity as the top-level path.
    auth_subject: Option<&str>,
) {
    use crate::turn_runner::{TurnForwardResult, TurnRunnerHandle, TurnStatus, run_gateway_turn};
    use futures_util::StreamExt as _;

    // WebSocket-only steering channel. The sender side is captured into the
    // forward closure; the receiver side is lent to the runner, which only
    // enters steering-drain mode when given a receiver.
    let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(32);

    // Reborrow the sender so the terminal-frame assembly below can still use it
    // after the forward closure (which owns a `&mut` into it) has been consumed.
    let sender_fwd = &mut *sender;

    let forward = move |handle: TurnRunnerHandle| async move {
        let TurnRunnerHandle {
            mut event_rx,
            cancel_token,
        } = handle;
        // Reconstruct partial content on cancellation by accumulating chunks.
        let mut accumulated_text = String::new();
        // Aggregate token usage across all LLM calls in this turn; the agent
        // emits one Usage event per LLM call when the provider surfaces usage.
        let mut total_input_tokens: Option<u64> = None;
        let mut total_output_tokens: Option<u64> = None;
        // Most recent absolute provider-reported prompt size (replaces on each
        // Usage; not accumulated) for the client's context bar.
        let mut last_input_tokens: Option<u64> = None;

        let mut cancel_drained = false;
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
                client_msg = receiver.next() => {
                    let text = match client_msg {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Ping(payload))) => {
                            if sender_fwd.send(Message::Pong(payload)).await.is_err() {
                                cancel_token.cancel();
                                break;
                            }
                            continue;
                        }
                        Some(Ok(Message::Pong(_))) => continue,
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
                        let _ = sender_fwd.send(Message::Text(err.to_string().into())).await;
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
                                &mut *sender_fwd,
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
                                let _ = sender_fwd.send(Message::Text(err.to_string().into())).await;
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
                                    let _ = sender_fwd.send(Message::Text(err.to_string().into())).await;
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                    let err = serde_json::json!({
                                        "type": "error",
                                        "message": "Running turn is no longer accepting steering messages",
                                        "code": "STEERING_CLOSED"
                                    });
                                    let _ = sender_fwd.send(Message::Text(err.to_string().into())).await;
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
                        let _ = sender_fwd.send(Message::Text(frame.to_string().into())).await;
                    }
                }
                _ = tick_websocket_ping(ping_interval) => {
                    if sender_fwd.send(Message::Ping(Vec::new().into())).await.is_err() {
                        cancel_token.cancel();
                        break;
                    }
                }
                event_opt = event_rx.recv() => {
                    let Some(event) = event_opt else { break };
                    if let TurnEvent::Chunk { delta } = &event {
                        accumulated_text.push_str(delta);
                    }
                    if let TurnEvent::Usage {
                        input_tokens,
                        cached_input_tokens: _,
                        output_tokens,
                        cost_usd: _,
                    } = &event {
                        if let Some(it) = *input_tokens {
                            total_input_tokens = Some(total_input_tokens.unwrap_or(0) + it);
                            last_input_tokens = Some(it);
                        }
                        if let Some(ot) = *output_tokens {
                            total_output_tokens = Some(total_output_tokens.unwrap_or(0) + ot);
                        }
                        continue;
                    }
                    if let Some(ws_msg) = turn_event_to_ws_frame(&event) {
                        let _ = sender_fwd.send(Message::Text(ws_msg.to_string().into())).await;
                    }
                }
            }
        }
        TurnForwardResult {
            total_input_tokens,
            total_output_tokens,
            last_input_tokens,
            accumulated_text,
        }
    };

    // The runner owns all state-level side effects (persistence, session state,
    // consolidation, agent_start/agent_end, tracing, cancellation registration).
    // WebSocket turns have no wall-clock timeout and always steer.
    let outcome = run_gateway_turn(
        state,
        agent,
        content,
        session_key,
        ws_memory,
        Some(&mut steering_rx),
        "wss",
        None,
        forward,
    )
    .await;

    // Terminal frame: exactly one done/aborted/error per turn.
    match outcome.status {
        TurnStatus::Success => {
            let done = serde_json::json!({
                "type": "done",
                "full_response": outcome.response_text,
                "input_tokens": outcome.total_input_tokens,
                "output_tokens": outcome.total_output_tokens,
                "tokens_used": outcome.total_tokens,
                "cost_usd": outcome.cost_usd,
                "model": outcome.model,
                "provider": outcome.provider,
                "max_context_tokens": outcome.max_context_tokens,
                "last_input_tokens": outcome.last_input_tokens,
            });
            let _ = sender.send(Message::Text(done.to_string().into())).await;
        }
        TurnStatus::Cancelled | TurnStatus::TimedOut => {
            // WebSocket turns never time out (timeout is None); TimedOut is
            // handled defensively like cancellation.
            let aborted = serde_json::json!({ "type": "aborted" });
            let _ = sender.send(Message::Text(aborted.to_string().into())).await;
        }
        TurnStatus::Error => {
            let Some(failure) = outcome.error.as_ref() else {
                return;
            };
            let err = ws_turn_failure_frame(
                &failure.diagnostic,
                failure.user_message.as_deref(),
                failure.user_message.is_some(),
            );
            let _ = sender.send(Message::Text(err.to_string().into())).await;

            // Broadcast error event (WS-specific component identity).
            let _ = state.event_tx.send(serde_json::json!({
                "type": "error",
                "component": "ws_chat",
                "message": err["message"],
            }));

            // Trace the failed turn so the doctor / replay tool sees the
            // failure mode and the turn_id can be cross-referenced with costs.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "model_provider": outcome.provider,
                        "model": outcome.model,
                        "session_key": session_key,
                        "error": zeroclaw_providers::sanitize_api_error(&failure.diagnostic),
                        "error_code": err["code"],
                        "trace_id": outcome.turn_id,
                    })),
                "gateway_ws_turn"
            );
        }
    }
}

/// Map a streamed turn event to its WebSocket frame. `Usage` events are never
/// framed — the transport consumes them for token aggregation instead.
fn turn_event_to_ws_frame(event: &TurnEvent) -> Option<serde_json::Value> {
    match event {
        TurnEvent::Usage { .. } => None,
        TurnEvent::Chunk { delta } => {
            Some(serde_json::json!({ "type": "chunk", "content": delta }))
        }
        TurnEvent::Thinking { delta } => {
            Some(serde_json::json!({ "type": "thinking", "content": delta }))
        }
        TurnEvent::ToolCall { id, name, args } => {
            Some(serde_json::json!({ "type": "tool_call", "id": id, "name": name, "args": args }))
        }
        TurnEvent::ToolResult {
            id, name, output, ..
        } => Some(serde_json::json!({
            "type": "tool_result",
            "id": id,
            "name": name,
            "output": output
        })),
        TurnEvent::ApprovalRequest {
            request_id,
            tool_name,
            arguments_summary,
            timeout_secs,
        } => Some(serde_json::json!({
            "type": "approval_request",
            "request_id": request_id,
            "tool": tool_name,
            "arguments_summary": arguments_summary,
            "timeout_secs": timeout_secs,
        })),
        TurnEvent::HistoryTrimmed {
            dropped_messages,
            kept_turns,
            reason,
        } => Some(history_trimmed_ws_frame(
            *dropped_messages,
            *kept_turns,
            reason,
        )),
        TurnEvent::Plan { entries } => Some(serde_json::json!({
            "type": "plan",
            "entries": entries,
        })),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn_runner::persist_conversation_messages;
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
    fn ws_guard_refcounts_and_drops_at_zero() {
        let ws_connections = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

        let g1 = WsConnectionGuard::new("gw_x".into(), Arc::clone(&ws_connections));
        let g2 = WsConnectionGuard::new("gw_x".into(), Arc::clone(&ws_connections));
        {
            let map = ws_connections.lock().unwrap();
            assert_eq!(map.get("gw_x"), Some(&2));
        }

        drop(g1);
        {
            let map = ws_connections.lock().unwrap();
            assert_eq!(map.get("gw_x"), Some(&1));
        }

        drop(g2);
        {
            let map = ws_connections.lock().unwrap();
            assert!(
                !map.contains_key("gw_x"),
                "last guard drop must remove the key"
            );
        }
    }

    #[test]
    fn ws_upgrade_failure_releases_pre_guard() {
        // Upgrade-failure path: `on_upgrade` spawns an async block that owns
        // the closure (with its moved-in pre-guard). If the handshake fails the
        // block ends without polling handle_socket; dropping the closure must
        // still release the lease.
        let ws_connections = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        {
            let _pre_ws_guard = WsConnectionGuard::new("gw_x".into(), Arc::clone(&ws_connections));
            assert_eq!(ws_connections.lock().unwrap().get("gw_x"), Some(&1));
            // Simulate the `Err(_)` branch: scope ends, the owning closure is
            // dropped, no handle_socket runs.
        }
        assert!(
            !ws_connections.lock().unwrap().contains_key("gw_x"),
            "handshake failure must not leak the pre-registered lease"
        );
    }

    #[test]
    fn turn_event_to_ws_frame_maps_every_variant_to_its_wire_shape() {
        use zeroclaw_api::plan::{PlanEntry, PlanPriority, PlanStatus};

        // Usage is aggregated by the forward closure and never frame-serialized.
        assert_eq!(
            turn_event_to_ws_frame(&TurnEvent::Usage {
                input_tokens: Some(1),
                cached_input_tokens: Some(2),
                output_tokens: Some(3),
                cost_usd: Some(0.0001),
            }),
            None,
        );

        let frame = turn_event_to_ws_frame(&TurnEvent::Chunk {
            delta: "Hello".into(),
        })
        .expect("chunk frame");
        assert_eq!(
            frame,
            serde_json::json!({ "type": "chunk", "content": "Hello" })
        );

        let frame = turn_event_to_ws_frame(&TurnEvent::Thinking {
            delta: "hmm".into(),
        })
        .expect("thinking frame");
        assert_eq!(
            frame,
            serde_json::json!({ "type": "thinking", "content": "hmm" })
        );

        let frame = turn_event_to_ws_frame(&TurnEvent::ToolCall {
            id: "call_1".into(),
            name: "ls".into(),
            args: serde_json::json!({ "path": "/tmp" }),
        })
        .expect("tool_call frame");
        assert_eq!(
            frame,
            serde_json::json!({
                "type": "tool_call",
                "id": "call_1",
                "name": "ls",
                "args": { "path": "/tmp" },
            })
        );

        let frame = turn_event_to_ws_frame(&TurnEvent::ToolResult {
            id: "call_1".into(),
            name: "ls".into(),
            output: "file.txt".into(),
            artifact: None,
        })
        .expect("tool_result frame");
        assert_eq!(
            frame,
            serde_json::json!({
                "type": "tool_result",
                "id": "call_1",
                "name": "ls",
                "output": "file.txt",
            }),
            "the typed artifact metadata must not leak into the wire frame"
        );

        let frame = turn_event_to_ws_frame(&TurnEvent::ApprovalRequest {
            request_id: "req_1".into(),
            tool_name: "exec".into(),
            arguments_summary: "run tests".into(),
            timeout_secs: 30,
        })
        .expect("approval_request frame");
        assert_eq!(
            frame,
            serde_json::json!({
                "type": "approval_request",
                "request_id": "req_1",
                "tool": "exec",
                "arguments_summary": "run tests",
                "timeout_secs": 30,
            })
        );

        // HistoryTrimmed must project exactly like the dedicated helper it calls.
        let frame = turn_event_to_ws_frame(&TurnEvent::HistoryTrimmed {
            dropped_messages: 4,
            kept_turns: 2,
            reason: "context window".into(),
        })
        .expect("history_trimmed frame");
        assert_eq!(frame, history_trimmed_ws_frame(4, 2, "context window"));
        assert_eq!(
            frame,
            serde_json::json!({
                "type": "history_trimmed",
                "dropped_messages": 4,
                "kept_turns": 2,
                "reason": "context window",
            })
        );

        let frame = turn_event_to_ws_frame(&TurnEvent::Plan {
            entries: vec![PlanEntry {
                content: "step one".into(),
                status: PlanStatus::Pending,
                priority: PlanPriority::Medium,
                active_form: None,
            }],
        })
        .expect("plan frame");
        assert_eq!(
            frame,
            serde_json::json!({
                "type": "plan",
                "entries": [{
                    "content": "step one",
                    "status": "pending",
                    "priority": "medium",
                }],
            })
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

    // The mid-turn `client_msg` arm in `forward_fut`
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
