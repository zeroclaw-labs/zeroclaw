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
    // Snapshot the session's turn-completion version *before* loading
    // history below. This connection holds no `session_queue` permit yet,
    // so this read and the `backend.load` below are not atomic with any
    // other connection's turn — but `bump_turn_version` only ever runs
    // *after* that turn's messages are fully persisted (see its call sites
    // in `process_chat_message`), on every completion path. So a version
    // this read can observe is always already reflected in whatever
    // `backend.load` returns: the ordering below can only make our loaded
    // messages as-new-as-or-newer-than `seen_version` implies, never
    // staler. Worst case a redundant rehydrate on the first prompt; never a
    // wrongly-skipped one.
    let mut seen_version: u64 = current_turn_version(&state.session_turn_versions, &session_key);
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
                let content = parsed["content"].as_str().unwrap_or("").to_string();
                if !content.is_empty() {
                    // Capture the deletion generation *before* awaiting the
                    // permit: the comparison after acquisition is what
                    // distinguishes "deleted while I queued" from "this
                    // session has simply never been written to disk yet".
                    let deletion_generation =
                        state.session_lifecycle.deletion_generation(&session_key);
                    // Acquire the permit *first*: only once we hold it is it
                    // guaranteed that any other connection's turn for this
                    // session has fully finished (cancel token removed,
                    // version bumped, messages persisted — see the bump
                    // site in `process_chat_message`). A liveness snapshot
                    // taken before the acquire (the previous approach) is
                    // TOCTOU-prone: it can read "not live" because A hasn't
                    // registered its cancel token yet, even though A holds
                    // the permit and will complete before we get it.
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
                    // The session may have been deleted while this prompt
                    // sat behind another turn on the permit above. Reject it
                    // rather than starting provider/tool execution for a
                    // session that no longer exists.
                    if session_deleted_while_queued(&state, &session_key, deletion_generation) {
                        let err = serde_json::json!({
                            "type": "error",
                            "message": "Session was deleted while this prompt was queued",
                            "code": "SESSION_DELETED"
                        });
                        let _ = sender.send(Message::Text(err.to_string().into())).await;
                        return;
                    }
                    if reject_prompt_after_failed_persistence(&state, &mut agent, &session_key) {
                        let err = serde_json::json!({
                            "type": "error",
                            "message": "A previous turn on this session could not be saved; \
                                        its transcript is incomplete. Retry your message.",
                            "code": "SESSION_PERSISTENCE_FAILED"
                        });
                        let _ = sender.send(Message::Text(err.to_string().into())).await;
                        return;
                    }
                    // Rehydrate iff a turn (this connection's own earlier
                    // one, or a different connection's) has completed since
                    // our `Agent` was last known current. The `seen_version`
                    // update that matters lives after `process_chat_message`
                    // below, unconditionally: it always re-derives the
                    // correct value (whether or not we rehydrated here, and
                    // even on `process_chat_message`'s early-return path,
                    // where no bump happens and it just re-reads the
                    // unchanged version), so there is nothing to track in
                    // between.
                    if current_turn_version(&state.session_turn_versions, &session_key)
                        != seen_version
                        && let Some(ref backend) = state.session_backend
                        && let Some(zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
                            dropped_messages,
                            kept_turns,
                            reason,
                        }) =
                            rehydrate_agent_from_backend(backend.as_ref(), &mut agent, &session_key)
                    {
                        let frame = history_trimmed_ws_frame(dropped_messages, kept_turns, &reason);
                        let _ = sender.send(Message::Text(frame.to_string().into())).await;
                    }
                    process_chat_message(
                        &state,
                        &mut agent,
                        &mut sender,
                        &mut receiver,
                        &mut approval_event_rx,
                        &pending_approvals,
                        &ws_memory,
                        &content,
                        &session_key,
                        &session_id,
                        auth_subject.as_deref(),
                    )
                    .await;
                    // This connection's own turn just bumped the version;
                    // track it so the next prompt on this connection does
                    // not redundantly rehydrate.
                    seen_version = current_turn_version(&state.session_turn_versions, &session_key);
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

                // Capture the deletion generation *before* awaiting the
                // permit — see the first-message path for why comparing
                // generations, not probing existence, is what separates a
                // deleted session from a not-yet-written one.
                let deletion_generation =
                    state.session_lifecycle.deletion_generation(&session_key);
                // Acquire the permit *first* — see the comment at the other
                // `session_queue.acquire` call site above (the first-message
                // path) for why a pre-acquire liveness snapshot is TOCTOU-prone
                // and why checking the turn-completion version under the
                // permit closes that window.
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
                // The session may have been deleted while this prompt sat
                // behind another turn on the permit above. Reject it rather
                // than starting provider/tool execution for a session that
                // no longer exists.
                if session_deleted_while_queued(&state, &session_key, deletion_generation) {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": "Session was deleted while this prompt was queued",
                        "code": "SESSION_DELETED"
                    });
                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                    continue;
                }
                if reject_prompt_after_failed_persistence(&state, &mut agent, &session_key) {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": "A previous turn on this session could not be saved; \
                                    its transcript is incomplete. Retry your message.",
                        "code": "SESSION_PERSISTENCE_FAILED"
                    });
                    let _ = sender.send(Message::Text(err.to_string().into())).await;
                    continue;
                }
                // Rehydrate iff a turn has completed since our `Agent` was
                // last known current. See the matching comment at the other
                // `session_queue.acquire` call site for why the
                // `seen_version` update that matters is the unconditional
                // one after `process_chat_message` below, not here.
                if current_turn_version(&state.session_turn_versions, &session_key) != seen_version
                    && let Some(ref backend) = state.session_backend
                    && let Some(zeroclaw_api::agent::TurnEvent::HistoryTrimmed {
                        dropped_messages,
                        kept_turns,
                        reason,
                    }) = rehydrate_agent_from_backend(backend.as_ref(), &mut agent, &session_key)
                {
                    let frame = history_trimmed_ws_frame(dropped_messages, kept_turns, &reason);
                    let _ = sender.send(Message::Text(frame.to_string().into())).await;
                }

                process_chat_message(
                    &state,
                    &mut agent,
                    &mut sender,
                    &mut receiver,
                    &mut approval_event_rx,
                    &pending_approvals,
                    &ws_memory,
                    &content,
                    &session_key,
                    &session_id,
                        auth_subject.as_deref(),
                )
                .await;
                // This connection's own turn just bumped the version; track
                // it so the next prompt on this connection does not
                // redundantly rehydrate.
                seen_version = current_turn_version(&state.session_turn_versions, &session_key);
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

/// Persist `messages` to `backend`. Returns `false` if any `append()` call
/// failed — a partial write that must not be certified as the session's
/// current authoritative history (see `bump_turn_version_after_persistence`,
/// which every caller must gate on this return value before advancing
/// `AppState::session_turn_versions`). Returns `true` when the session no
/// longer exists: there is nothing to persist, and that is not itself a
/// persistence failure.
fn persist_conversation_messages(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    session_key: &str,
    messages: &[zeroclaw_providers::ConversationMessage],
) -> bool {
    // if the user deleted the session between the turn starting and
    // the post-turn persistence, don't resurrect it. The `aborted` / `done`
    // / `error` frames are still sent to the client; we just refuse to
    // re-create the row that `DELETE /api/sessions/{id}` just wiped.
    if !backend.session_exists(session_key) {
        return true;
    }
    let mut all_persisted = true;
    for message in messages {
        let zeroclaw_providers::ConversationMessage::Chat(message) = message else {
            continue;
        };
        if message.role == "system" {
            continue;
        }
        if let Err(e) = backend.append(session_key, message) {
            all_persisted = false;
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "session_key": session_key,
                        "error": format!("{e}"),
                    })),
                "failed to persist conversation message"
            );
        }
    }
    all_persisted
}

/// Bump `session_key`'s turn-completion version, but only when this
/// completion path's backend persistence actually succeeded. Skipping the
/// bump on a failed/partial `append()` keeps `session_turn_versions` from
/// certifying transcript state that a reconnecting or queued connection
/// cannot actually load — see `bump_turn_version`'s doc comment for the
/// underlying ordering invariant this preserves.
fn bump_turn_version_after_persistence(
    session_turn_versions: &std::sync::Mutex<std::collections::HashMap<String, u64>>,
    session_key: &str,
    persisted_ok: bool,
    lifecycle: &crate::session_lifecycle::SessionLifecycle,
) {
    if persisted_ok {
        bump_turn_version(session_turn_versions, session_key);
    } else {
        // Record the failure as well as withholding the bump. An unchanged
        // version reads to a queued connection as "no turn completed", so it
        // would skip rehydration and run against history the failed append
        // was meant to extend.
        lifecycle.record_persistence_failure(session_key);
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"session_key": session_key})),
            "not bumping turn-completion version after failed conversation persistence"
        );
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

/// Fail closed when the WebSocket viewer can no longer answer supervised-mode
/// tool prompts. Detaching the viewer must not cancel the whole turn, but it
/// also must not leave an approval parked until its normal timeout or allow a
/// later response from a dead connection to authorize the tool.
fn deny_pending_ws_approvals(pending_approvals: &PendingApprovals) -> usize {
    let pending: Vec<_> = pending_approvals.lock().drain().collect();
    let count = pending.len();
    for (_, response_tx) in pending {
        let _ = response_tx.send(ChannelApprovalResponse::Deny);
    }
    count
}

fn detach_ws_viewer(client_attached: &mut bool, pending_approvals: &PendingApprovals) -> usize {
    *client_attached = false;
    deny_pending_ws_approvals(pending_approvals)
}

/// Register the canonical cancellation token for a session without replacing
/// an already-running turn. A second WebSocket may reconnect to the same
/// session while the original turn is detached, so replacement would make the
/// abort and session-state endpoints observe the wrong turn.
fn register_cancel_token(
    cancel_tokens: &std::sync::Mutex<
        std::collections::HashMap<String, tokio_util::sync::CancellationToken>,
    >,
    session_key: &str,
    cancel_token: tokio_util::sync::CancellationToken,
) -> bool {
    use std::collections::hash_map::Entry;

    let mut tokens = cancel_tokens.lock().expect("cancel_tokens lock poisoned");
    match tokens.entry(session_key.to_string()) {
        Entry::Vacant(entry) => {
            entry.insert(cancel_token);
            true
        }
        Entry::Occupied(_) => false,
    }
}

/// True when `DELETE /api/sessions/{id}` removed this session while the
/// caller sat queued on the `session_queue` permit.
///
/// Compares the deletion generation captured *before* the wait against the
/// current one, rather than probing the backend for existence. An existence
/// probe cannot express this: `SessionStore::session_exists` is a file-presence
/// check and a JSONL session file is not created until its first append, so
/// "absent" covers both *deleted* and *never written yet*. Probing therefore
/// rejected the first prompt of every new session as `SESSION_DELETED`.
///
/// Generation comparison also catches delete-then-recreate, where the session
/// exists at both ends of the wait but the caller's history is stale anyway.
fn session_deleted_while_queued(
    state: &AppState,
    session_key: &str,
    captured: crate::session_lifecycle::DeletionGeneration,
) -> bool {
    state.session_lifecycle.deleted_since(session_key, captured)
}

/// Reject a queued prompt whose session has a turn that failed to persist.
///
/// Withholding the turn-version bump on a failed append keeps a queued
/// connection from *believing* a turn completed, but that is not by itself
/// protective: with the version unchanged, the queued connection's
/// `seen_version` comparison comes out equal, so it skips rehydration and
/// runs its prompt against pre-turn history — precisely the transcript the
/// failed append was supposed to extend. The result would be a silently
/// divergent conversation rather than a visible error.
///
/// The gateway cannot repair the backend, so it does the next best thing:
/// fail this prompt loudly, and re-seed the connection's `Agent` from
/// whatever the backend actually holds. That re-establishes agreement
/// between memory and storage, so the session is usable again on the next
/// prompt instead of being wedged for the life of the connection. Returns
/// `true` when the caller should reject the prompt.
fn reject_prompt_after_failed_persistence(
    state: &AppState,
    agent: &mut zeroclaw_runtime::agent::Agent,
    session_key: &str,
) -> bool {
    if !state.session_lifecycle.persistence_failed(session_key) {
        return false;
    }
    // Re-seed from storage before clearing the flag: the in-memory history
    // is known to disagree with the backend, and only a successful reload
    // makes it safe to accept another prompt on this session.
    if let Some(ref backend) = state.session_backend {
        let _ = rehydrate_agent_from_backend(backend.as_ref(), agent, session_key);
    }
    state
        .session_lifecycle
        .clear_persistence_failure(session_key);
    true
}

/// Rehydrate a connection-scoped `Agent`'s history from the session backend.
///
/// Called under the `session_queue` permit, once this connection has
/// observed (via `AppState::session_turn_versions`) that some turn — its own
/// earlier one, or a different connection's — has completed since this
/// `Agent`'s history was last known current. `process_chat_message` bumps
/// the version, removes the cancel token, and persists the turn's messages
/// all before it returns and releases the permit its caller holds, so by the
/// time this runs, `backend.load` is guaranteed to be the authoritative,
/// up-to-date transcript. Clearing and reseeding (rather than appending)
/// avoids duplicating whatever this connection's `Agent` already held and
/// guarantees the next turn runs against post-turn history, never a stale
/// snapshot captured earlier (at connect time, or after this connection's
/// own last turn).
fn rehydrate_agent_from_backend(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    agent: &mut zeroclaw_runtime::agent::Agent,
    session_key: &str,
) -> Option<zeroclaw_api::agent::TurnEvent> {
    let messages = backend.load(session_key);
    agent.clear_history();
    agent.seed_history_with_event(&messages)
}

/// Read `session_key`'s current turn-completion version — 0 if no turn has
/// ever completed for this session. See `AppState::session_turn_versions`
/// for the bump site and the invariant this backs.
fn current_turn_version(
    session_turn_versions: &std::sync::Mutex<std::collections::HashMap<String, u64>>,
    session_key: &str,
) -> u64 {
    session_turn_versions
        .lock()
        .expect("session_turn_versions lock poisoned")
        .get(session_key)
        .copied()
        .unwrap_or(0)
}

/// Bump `session_key`'s turn-completion version by one.
///
/// Must be called on every `process_chat_message` completion path — but
/// only *after* that path's backend persistence (if any) has finished, never
/// before. A connection's connect-time `current_turn_version` read (see
/// `handle_socket`) happens without holding the `session_queue` permit, so
/// it can race an in-flight turn's completion; the only thing that keeps
/// that race safe is the invariant "a version this bumps to is never
/// observable before the messages behind it are persisted". Bumping before
/// persistence would let a new connection read the bumped version, then
/// load history that doesn't include this turn yet, and be indistinguishable
/// from a connection that *did* rehydrate onto current history even though
/// its seed is stale — silently resurrecting the pre-A-history bug this
/// version scheme exists to close.
pub(crate) fn bump_turn_version(
    session_turn_versions: &std::sync::Mutex<std::collections::HashMap<String, u64>>,
    session_key: &str,
) {
    let mut versions = session_turn_versions
        .lock()
        .expect("session_turn_versions lock poisoned");
    let next = versions.get(session_key).copied().unwrap_or(0) + 1;
    versions.insert(session_key.to_string(), next);
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
// Generic over the transport halves (rather than tied to axum's concrete
// `SplitSink<WebSocket, _>` / `SplitStream<WebSocket>`) so the two production
// call sites in `handle_socket` are unaffected, but the session-queue /
// cancel-token / rehydration logic below can be driven directly by tests with
// in-process fakes — a real two-socket regression needs two independent
// `process_chat_message` calls racing on the same `AppState`, which a real
// WebSocket transport cannot orchestrate deterministically.
#[allow(clippy::too_many_arguments)]
async fn process_chat_message<Snk, Rcv, RcvErr>(
    state: &AppState,
    agent: &mut zeroclaw_runtime::agent::Agent,
    sender: &mut Snk,
    receiver: &mut Rcv,
    approval_event_rx: &mut tokio::sync::mpsc::Receiver<zeroclaw_api::agent::TurnEvent>,
    pending_approvals: &PendingApprovals,
    ws_memory: &Option<Arc<dyn zeroclaw_memory::Memory>>,
    content: &str,
    session_key: &str,
    session_id: &str,
    // Transport-authenticated approval subject (paired-token hash), threaded so a
    // mid-turn SOP approval frame carries the same identity as the top-level path.
    auth_subject: Option<&str>,
) where
    Snk: futures_util::Sink<Message> + Unpin,
    Rcv: futures_util::Stream<Item = Result<Message, RcvErr>> + Unpin,
{
    use futures_util::StreamExt as _;
    use zeroclaw_runtime::agent::TurnEvent;

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

    // The cancellation-token map is the canonical process-local authority for
    // a live turn. Register atomically before publishing any start side effects;
    // a reconnected WebSocket must not replace the original token.
    let cancel_token = tokio_util::sync::CancellationToken::new();
    if !register_cancel_token(&state.cancel_tokens, session_key, cancel_token.clone()) {
        let err = serde_json::json!({
            "type": "error",
            "message": zeroclaw_runtime::i18n::get_required_cli_string(
                "cli-ws-session-turn-active"
            ),
            "code": "SESSION_TURN_ACTIVE"
        });
        let _ = sender.send(Message::Text(err.to_string().into())).await;
        return;
    }

    // A detached turn (viewer disconnected) is no longer observed by any
    // client-facing select arm, so without this it would keep running
    // provider/tool calls straight through a gateway shutdown. Subscribe to
    // the same listener watch channel `run_gateway`'s accept loop uses and
    // cancel this turn exactly like an explicit abort when it fires — the
    // existing `was_cancelled` handling below then persists partial output
    // and releases the token/permit the same way it always has. Check the
    // already-shut-down case up front: a fresh `subscribe()` only observes
    // *future* changes, so a turn that starts after shutdown was signalled
    // would otherwise never see it.
    let mut shutdown_rx = state.shutdown_tx.subscribe();
    if *shutdown_rx.borrow() {
        cancel_token.cancel();
    }

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

    let forward_fut = async {
        let mut cancel_drained = false;
        let mut client_attached = true;
        let mut approval_events_open = true;
        let mut shutdown_drained = false;
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
                _ = shutdown_rx.changed(), if !shutdown_drained => {
                    // Disable this arm after the first observed change —
                    // `shutdown_tx` only ever flips false -> true once per
                    // process lifetime, so a second `changed()` would just
                    // await forever, but disabling keeps the biased ordering
                    // above cheap to reason about (never re-polled once
                    // resolved, same as the `cancel_token` arm).
                    shutdown_drained = true;
                    if *shutdown_rx.borrow() {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_attrs(::serde_json::json!({"session_key": session_key})),
                            "gateway shutdown observed; cancelling live turn"
                        );
                        cancel_token.cancel();
                    }
                }
                client_msg = receiver.next(), if client_attached => {
                    // A WebSocket is a viewer/controller for the turn, not its
                    // owner. Route changes, browser sleep, and transient
                    // network loss therefore detach the client without firing
                    // the turn's cancellation token. Disable this select arm
                    // after detach so a closed stream cannot recreate the
                    // immediately-ready hot loop; `event_rx` remains active and
                    // is drained until the agent finishes naturally.
                    let text = match client_msg {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                            let denied =
                                detach_ws_viewer(&mut client_attached, pending_approvals);
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note,
                                )
                                .with_attrs(::serde_json::json!({
                                    "session_key": session_key,
                                    "pending_approvals_denied": denied,
                                })),
                                "WebSocket viewer detached; agent turn continues"
                            );
                            continue;
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
                approval = approval_event_rx.recv(), if approval_events_open => {
                    let Some(event) = approval else {
                        // Disable a closed receiver: `recv()` would otherwise
                        // remain immediately ready and starve `event_rx` in
                        // this biased select while the turn winds down.
                        approval_events_open = false;
                        continue;
                    };
                    if let TurnEvent::ApprovalRequest {
                        request_id,
                        tool_name,
                        arguments_summary,
                        timeout_secs,
                    } = event {
                        if !client_attached {
                            if let Some(tx) = pending_approvals.lock().remove(&request_id) {
                                let _ = tx.send(ChannelApprovalResponse::Deny);
                            }
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
                            detach_ws_viewer(&mut client_attached, pending_approvals);
                        }
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
                            if !client_attached {
                                if let Some(tx) = pending_approvals.lock().remove(&request_id) {
                                    let _ = tx.send(ChannelApprovalResponse::Deny);
                                }
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
                    if client_attached
                        && sender
                            .send(Message::Text(ws_msg.to_string().into()))
                            .await
                            .is_err()
                    {
                        detach_ws_viewer(&mut client_attached, pending_approvals);
                    }
                }
            }
        }
    };

    let (result, ()) = tokio::join!(turn_fut, forward_fut);

    // Enter the finalizing state *before* dropping the cancel token, so the
    // session is continuously authoritative: the token covers streaming, this
    // guard covers persistence and turn-version disposition, and they overlap
    // rather than leaving an `idle` gap for the session-state endpoint to
    // expose. The guard releases on drop, so every early return and error
    // path below still clears it.
    let _finalizing = crate::session_lifecycle::FinalizingGuard::new(
        state.session_lifecycle.as_ref(),
        session_key,
    );

    // ── Remove cancel token (turn finished) ──────────────────────
    {
        state
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .remove(session_key);
    }

    // The turn-completion version (`AppState::session_turn_versions`) is
    // bumped once on each completion path below (cancelled / success /
    // error) via `bump_turn_version_after_persistence`, which only advances
    // the version *after* that path's backend persistence has both finished
    // and succeeded — a connection's connect-time version read does not hold
    // the `session_queue` permit (see `handle_socket`), so it can race an
    // in-flight turn; bumping before persistence finishes would let it
    // observe the new version while the messages behind it are still
    // unpersisted, and bumping despite a failed/partial `append()` would let
    // it adopt that partial write as the authoritative transcript. See
    // `bump_turn_version`'s doc comment for the full ordering invariant.

    // Check if this turn was cancelled. `turn_streamed` propagates
    // `ToolLoopCancelled` through anyhow, so we detect it here.
    let was_cancelled = match &result {
        Err(e) => zeroclaw_runtime::agent::loop_::is_tool_loop_cancelled(&e.error),
        Ok(_) => false,
    };

    if was_cancelled {
        let mut persisted_ok = true;
        if let Some(ref backend) = state.session_backend {
            let still_exists = backend.session_exists(session_key);
            if still_exists {
                match &result {
                    Err(error) if !error.new_messages.is_empty() => {
                        persisted_ok = persist_conversation_messages(
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
                            if backend.session_exists(session_key)
                                && let Err(e) = backend.append(session_key, &assistant_msg)
                            {
                                persisted_ok = false;
                                ::zeroclaw_log::record!(
                                    ERROR,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Fail
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "session_key": session_key,
                                            "error": format!("{e}"),
                                        })
                                    ),
                                    "failed to persist interrupted-turn marker message"
                                );
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
                        if backend.session_exists(session_key)
                            && let Err(e) = backend.append(session_key, &assistant_msg)
                        {
                            persisted_ok = false;
                            ::zeroclaw_log::record!(
                                ERROR,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Fail
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "session_key": session_key,
                                    "error": format!("{e}"),
                                })),
                                "failed to persist interrupted-turn marker message"
                            );
                        }
                    }
                }
            }
        }
        // Persistence above (if any) is done — bump only if it succeeded.
        bump_turn_version_after_persistence(
            &state.session_turn_versions,
            session_key,
            persisted_ok,
            state.session_lifecycle.as_ref(),
        );

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

        return;
    }

    match result {
        Ok(outcome) => {
            let persisted_ok = state.session_backend.as_ref().is_none_or(|backend| {
                persist_conversation_messages(backend.as_ref(), session_key, &outcome.new_messages)
            });
            // Persistence above (if any) is done — bump only if it succeeded.
            bump_turn_version_after_persistence(
                &state.session_turn_versions,
                session_key,
                persisted_ok,
                state.session_lifecycle.as_ref(),
            );

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
            let persisted_ok = if e.new_messages.is_empty() {
                true
            } else {
                state.session_backend.as_ref().is_none_or(|backend| {
                    persist_conversation_messages(backend.as_ref(), session_key, &e.new_messages)
                })
            };
            // Persistence above (if any) is done — bump only if it succeeded.
            bump_turn_version_after_persistence(
                &state.session_turn_versions,
                session_key,
                persisted_ok,
                state.session_lifecycle.as_ref(),
            );

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use zeroclaw_infra::session_backend::SessionBackend as _;

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
    fn second_connection_cannot_replace_the_running_turn_cancel_token() {
        let tokens = std::sync::Mutex::new(std::collections::HashMap::new());
        let original = tokio_util::sync::CancellationToken::new();
        let reconnect = tokio_util::sync::CancellationToken::new();

        assert!(register_cancel_token(
            &tokens,
            "gw_session",
            original.clone()
        ));
        assert!(!register_cancel_token(
            &tokens,
            "gw_session",
            reconnect.clone()
        ));

        let authoritative = tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .get("gw_session")
            .cloned()
            .expect("original token must remain registered");
        authoritative.cancel();

        assert!(original.is_cancelled());
        assert!(!reconnect.is_cancelled());
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

    // Regression coverage for detached viewers. The mid-turn `client_msg` arm
    // in `forward_fut` must classify stream-end / close / error frames as a
    // detach. The production arm then disables itself while continuing to
    // drain turn events; a bare `continue` would hot-loop, while cancellation
    // would incorrectly stop work merely because its viewer disappeared.
    #[derive(Debug, PartialEq, Eq)]
    enum DisconnectAction {
        Detach,
        Continue,
        ProcessText,
    }

    fn classify_client_msg(
        msg: Option<Result<axum::extract::ws::Message, &'static str>>,
    ) -> DisconnectAction {
        use axum::extract::ws::Message;
        match msg {
            Some(Ok(Message::Text(_))) => DisconnectAction::ProcessText,
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => DisconnectAction::Detach,
            _ => DisconnectAction::Continue,
        }
    }

    #[test]
    fn mid_turn_client_msg_detaches_on_stream_end_close_or_err() {
        use axum::extract::ws::Message;
        assert_eq!(classify_client_msg(None), DisconnectAction::Detach);
        assert_eq!(
            classify_client_msg(Some(Ok(Message::Close(None)))),
            DisconnectAction::Detach,
        );
        assert_eq!(
            classify_client_msg(Some(Err("io"))),
            DisconnectAction::Detach,
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
    fn mid_turn_detach_does_not_cancel_the_turn() {
        let token = tokio_util::sync::CancellationToken::new();
        let clone_for_turn = token.clone();
        let pending = new_pending_approvals();
        let mut client_attached = true;
        assert!(!clone_for_turn.is_cancelled());
        let action = classify_client_msg(None);
        assert_eq!(action, DisconnectAction::Detach);
        detach_ws_viewer(&mut client_attached, &pending);
        assert!(!client_attached);
        assert!(
            !clone_for_turn.is_cancelled(),
            "transport detach must leave the turn's explicit cancellation token live"
        );
    }

    #[tokio::test]
    async fn detached_viewer_keeps_bounded_turn_events_draining() {
        let token = tokio_util::sync::CancellationToken::new();
        let pending = new_pending_approvals();
        let mut client_attached = true;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);

        detach_ws_viewer(&mut client_attached, &pending);
        let producer = zeroclaw_spawn::spawn!(async move {
            event_tx.send("first").await.unwrap();
            event_tx.send("second").await.unwrap();
        });

        let drained = tokio::time::timeout(std::time::Duration::from_secs(1), async move {
            let mut events = Vec::new();
            while let Some(event) = event_rx.recv().await {
                events.push(event);
            }
            events
        })
        .await
        .expect("detached forward path must keep draining until the producer closes");

        producer.await.unwrap();
        assert_eq!(drained, vec!["first", "second"]);
        assert!(!token.is_cancelled());
    }

    #[test]
    fn detached_viewer_denies_all_pending_approvals() {
        let pending = new_pending_approvals();
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let (second_tx, second_rx) = tokio::sync::oneshot::channel();
        pending.lock().insert("first".into(), first_tx);
        pending.lock().insert("second".into(), second_tx);

        assert_eq!(deny_pending_ws_approvals(&pending), 2);
        assert!(pending.lock().is_empty());
        assert_eq!(
            first_rx.blocking_recv().unwrap(),
            ChannelApprovalResponse::Deny
        );
        assert_eq!(
            second_rx.blocking_recv().unwrap(),
            ChannelApprovalResponse::Deny
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

    /// A `SessionBackend` whose `append` fails from a configurable call
    /// index onward, standing in for a real backend that partially writes a
    /// turn's messages before an I/O error (disk full, DB lock timeout, ...).
    struct PartiallyFailingAppendBackend {
        appended: std::sync::Mutex<Vec<String>>,
        call_count: std::sync::atomic::AtomicUsize,
        fail_at_call: usize,
    }

    impl zeroclaw_infra::session_backend::SessionBackend for PartiallyFailingAppendBackend {
        fn load(&self, _session_key: &str) -> Vec<zeroclaw_providers::ChatMessage> {
            Vec::new()
        }
        fn append(
            &self,
            session_key: &str,
            message: &zeroclaw_providers::ChatMessage,
        ) -> std::io::Result<()> {
            let call = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call >= self.fail_at_call {
                return Err(std::io::Error::other("simulated partial-append failure"));
            }
            self.appended.lock().unwrap().push(format!(
                "{session_key}:{}:{}",
                message.role, message.content
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
            true
        }
    }

    /// `persist_conversation_messages` must
    /// report a partial write instead of silently discarding the `append()`
    /// error, so its caller can gate the turn-completion version bump on it.
    #[test]
    fn persist_conversation_messages_reports_failure_on_partial_append() {
        use zeroclaw_providers::{ChatMessage, ConversationMessage};
        let backend = PartiallyFailingAppendBackend {
            appended: std::sync::Mutex::new(Vec::new()),
            call_count: std::sync::atomic::AtomicUsize::new(0),
            fail_at_call: 1, // the user message persists; the assistant reply fails
        };
        let messages = vec![
            ConversationMessage::Chat(ChatMessage::user("hi")),
            ConversationMessage::Chat(ChatMessage::assistant("reply")),
        ];

        let persisted_ok = persist_conversation_messages(&backend, "gw_partial", &messages);

        assert!(
            !persisted_ok,
            "a failed append must be reported to the caller, not swallowed"
        );
        assert_eq!(
            backend.appended.lock().unwrap().len(),
            1,
            "the message that did succeed is still best-effort persisted"
        );
    }

    /// A partial/failed `append()` during
    /// turn completion must not advance `session_turn_versions` — otherwise
    /// a queued or reconnecting connection can observe the bumped version,
    /// clear its `Agent` history, and rehydrate onto a transcript that is
    /// missing the messages the failed append never wrote.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn version_is_not_bumped_after_partial_append_failure() {
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        let backend = std::sync::Arc::new(PartiallyFailingAppendBackend {
            appended: std::sync::Mutex::new(Vec::new()),
            call_count: std::sync::atomic::AtomicUsize::new(0),
            fail_at_call: 1, // the user message persists; the assistant reply fails
        });
        state.session_backend = Some(backend.clone());
        let state = state;

        let session_id = "partial-append-session";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

        let mut agent = queue_test_agent(Box::new(ImmediateModelProvider("reply-content")));
        let mut sender = CollectSink(Vec::new());
        let mut receiver =
            futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
        let (_approval_tx, mut approval_rx) = tokio::sync::mpsc::channel(4);
        let pending = new_pending_approvals();

        process_chat_message(
            &state,
            &mut agent,
            &mut sender,
            &mut receiver,
            &mut approval_rx,
            &pending,
            &None,
            "prompt-with-partial-persistence-failure",
            &session_key,
            session_id,
            None,
        )
        .await;

        // The turn itself still completes and reports success to the
        // client — a persistence failure must not be surfaced as a turn
        // failure.
        let done = sender.0.iter().any(|frame| {
            serde_json::from_str::<serde_json::Value>(frame)
                .ok()
                .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
                .as_deref()
                == Some("done")
        });
        assert!(
            done,
            "the turn itself must still complete and notify the client: {:?}",
            sender.0
        );

        assert_eq!(
            current_turn_version(&state.session_turn_versions, &session_key),
            0,
            "session_turn_versions must not advance after a failed/partial append"
        );
        assert!(
            state
                .cancel_tokens
                .lock()
                .unwrap()
                .get(&session_key)
                .is_none(),
            "the turn must still release its cancel token even though persistence failed"
        );
        assert!(
            state.session_lifecycle.persistence_failed(&session_key),
            "the failed append must be recorded so the next writer can be stopped"
        );
    }

    /// The withheld version bump alone does not protect the next writer: an
    /// unchanged version reads to a queued connection as "no turn
    /// completed", so it skips rehydration and would run against the
    /// pre-turn history the failed append was meant to extend. The next
    /// prompt must instead be rejected, and the connection's `Agent`
    /// re-seeded from what the backend actually holds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_prompt_is_rejected_after_failed_persistence() {
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        let backend = std::sync::Arc::new(PartiallyFailingAppendBackend {
            appended: std::sync::Mutex::new(Vec::new()),
            call_count: std::sync::atomic::AtomicUsize::new(0),
            fail_at_call: 1,
        });
        state.session_backend = Some(backend.clone());
        let state = state;

        let session_id = "reject-after-failed-persistence";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

        // Socket A's turn fails to persist fully.
        state
            .session_lifecycle
            .record_persistence_failure(&session_key);

        // Socket B is a *different* connection whose `seen_version` matches
        // the (correctly un-bumped) current version, so the rehydrate check
        // below would not fire on its own.
        let seen_version_b = current_turn_version(&state.session_turn_versions, &session_key);
        assert_eq!(
            seen_version_b, 0,
            "precondition: the failed append must not have bumped the version"
        );

        let mut agent_b = queue_test_agent(Box::new(ImmediateModelProvider("b-reply")));
        let rejected = reject_prompt_after_failed_persistence(&state, &mut agent_b, &session_key);

        assert!(
            rejected,
            "a prompt queued behind a turn that failed to persist must be rejected, \
             not silently run against stale history"
        );
        assert!(
            !state.session_lifecycle.persistence_failed(&session_key),
            "the flag must clear once the agent has been re-seeded, so the session \
             recovers instead of staying wedged for the life of the connection"
        );
        assert!(
            !reject_prompt_after_failed_persistence(&state, &mut agent_b, &session_key),
            "a second prompt must be accepted once storage and memory agree again"
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

    // ── Production-boundary two-socket regression ────────────────────────
    //
    // A prompt B arriving while turn A is live must
    // (1) be queued/attached rather than run concurrently with A, (2) run
    // against A's completed history once it does run — never the pre-A
    // snapshot its connection-scoped `Agent` was seeded with at connect
    // time — and (3) never displace A's cancel token as the session's abort
    // authority while A is still live. Unlike the `register_cancel_token`
    // map-helper tests above, this drives the real `process_chat_message`
    // (the same function both `handle_socket` call sites use), the real
    // `AppState::session_queue` / `cancel_tokens` / `session_turn_versions`,
    // and the real `current_turn_version` / `bump_turn_version` /
    // `rehydrate_agent_from_backend` added for this fix, from two
    // independent tasks racing on one shared session — the interleaving a
    // real two-socket reconnect produces, driven deterministically instead
    // of by timing.

    /// In-memory `SessionBackend` that also tracks which sessions have a
    /// `set_session_state` (turn started) or `append` call, mirroring the
    /// SQLite backend's `session_metadata` row: a session can be "known" —
    /// and therefore eligible for `persist_conversation_messages` — before
    /// its first message is appended.
    #[derive(Default)]
    struct QueueTestSessionBackend {
        messages: std::sync::Mutex<
            std::collections::HashMap<String, Vec<zeroclaw_providers::ChatMessage>>,
        >,
        known: std::sync::Mutex<std::collections::HashSet<String>>,
    }

    impl zeroclaw_infra::session_backend::SessionBackend for QueueTestSessionBackend {
        fn load(&self, session_key: &str) -> Vec<zeroclaw_providers::ChatMessage> {
            self.messages
                .lock()
                .unwrap()
                .get(session_key)
                .cloned()
                .unwrap_or_default()
        }
        fn append(
            &self,
            session_key: &str,
            message: &zeroclaw_providers::ChatMessage,
        ) -> std::io::Result<()> {
            self.messages
                .lock()
                .unwrap()
                .entry(session_key.to_string())
                .or_default()
                .push(message.clone());
            self.known.lock().unwrap().insert(session_key.to_string());
            Ok(())
        }
        fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
            Ok(self
                .messages
                .lock()
                .unwrap()
                .get_mut(session_key)
                .is_some_and(|v| v.pop().is_some()))
        }
        fn list_sessions(&self) -> Vec<String> {
            self.messages.lock().unwrap().keys().cloned().collect()
        }
        fn session_exists(&self, session_key: &str) -> bool {
            self.known.lock().unwrap().contains(session_key)
        }
        fn set_session_state(
            &self,
            session_key: &str,
            _state: &str,
            _turn_id: Option<&str>,
        ) -> std::io::Result<()> {
            self.known.lock().unwrap().insert(session_key.to_string());
            Ok(())
        }
        fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
            self.messages.lock().unwrap().remove(session_key);
            Ok(self.known.lock().unwrap().remove(session_key))
        }
    }

    /// Stands in for turn A: its reply never arrives on its own, so the test
    /// can hold A "mid-turn" deterministically and then interrupt it with a
    /// real abort rather than guessing at timing.
    struct StuckModelProvider;

    #[async_trait::async_trait]
    impl zeroclaw_providers::ModelProvider for StuckModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            std::future::pending::<()>().await;
            unreachable!("this future is only ever dropped via cancellation")
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for StuckModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "StuckModelProvider"
        }
    }

    /// Stands in for turn B: replies immediately, so any delay it observes
    /// comes entirely from `session_queue` contention, not the model call.
    struct ImmediateModelProvider(&'static str);

    #[async_trait::async_trait]
    impl zeroclaw_providers::ModelProvider for ImmediateModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(self.0.to_string())
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ImmediateModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ImmediateModelProvider"
        }
    }

    /// A connection-scoped `Agent`, built the way each WebSocket builds its
    /// own — fresh, empty history — but with an in-process model_provider
    /// instead of a live-config-resolved HTTP one, so the turn itself is
    /// deterministic and network-free.
    fn queue_test_agent(
        model_provider: Box<dyn zeroclaw_providers::ModelProvider>,
    ) -> zeroclaw_runtime::agent::Agent {
        zeroclaw_runtime::agent::Agent::builder()
            .model_provider(model_provider)
            .tools(Vec::new())
            .memory(std::sync::Arc::new(zeroclaw_memory::NoneMemory::new(
                "none",
            )))
            .observer(std::sync::Arc::new(
                zeroclaw_runtime::observability::NoopObserver,
            ))
            .tool_dispatcher(Box::new(
                zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher,
            ))
            .workspace_dir(std::env::temp_dir())
            .build()
            .expect("test agent builds with a minimal in-process model_provider")
    }

    async fn queue_test_response_json(response: axum::response::Response) -> serde_json::Value {
        use http_body_util::BodyExt as _;
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        serde_json::from_slice(&body).expect("valid json response")
    }

    /// Common post-conditions for both two-socket regressions below: A's
    /// abort lands on the exact token A registered, B rehydrates onto A's
    /// completed history (never the pre-A snapshot), the persisted
    /// transcript orders A's turn before B's, both turns release their
    /// cancel token, and B's own turn still runs to completion.
    fn assert_b_rehydrated_after_a_aborted(
        state: &AppState,
        backend: &QueueTestSessionBackend,
        session_key: &str,
        agent_b: &zeroclaw_runtime::agent::Agent,
        sender_b: &CollectSink,
    ) {
        let agent_b_saw_prompt_a = agent_b.history().iter().any(|m| {
            matches!(
                m,
                zeroclaw_providers::ConversationMessage::Chat(c) if c.content.contains("prompt-A")
            )
        });
        assert!(
            agent_b_saw_prompt_a,
            "B's rehydrated Agent must carry turn A's completed history: {:?}",
            agent_b.history()
        );

        let persisted = backend.load(session_key);
        let persisted_text: Vec<&str> = persisted.iter().map(|m| m.content.as_str()).collect();
        let a_index = persisted_text
            .iter()
            .position(|c| c.contains("prompt-A"))
            .expect("backend must hold A's prompt");
        let b_index = persisted_text
            .iter()
            .position(|c| c.contains("prompt-B"))
            .expect("backend must hold B's prompt");
        assert!(
            a_index < b_index,
            "A's turn must precede B's in the persisted transcript: {persisted_text:?}"
        );

        assert!(
            state.cancel_tokens.lock().unwrap().is_empty(),
            "both turns must release their cancel token on completion"
        );

        let b_completed = sender_b.0.iter().any(|frame| {
            serde_json::from_str::<serde_json::Value>(frame)
                .ok()
                .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
                .as_deref()
                == Some("done")
        });
        assert!(
            b_completed,
            "B's own turn must still run to completion after rehydration: {:?}",
            sender_b.0
        );
    }

    /// Benign interleaving: B does not arrive until A has already registered
    /// as the session's live turn (mirrors a straightforward "second prompt
    /// while the first is running" reconnect).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn queued_prompt_rehydrates_from_post_turn_a_history_and_keeps_a_abort_authority() {
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        let backend = std::sync::Arc::new(QueueTestSessionBackend::default());
        state.session_backend = Some(backend.clone());
        let state = state;

        let session_id = "shared-session";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

        let mut agent_a = queue_test_agent(Box::new(StuckModelProvider));
        let mut agent_b = queue_test_agent(Box::new(ImmediateModelProvider("B-reply")));

        // ── Turn A: the first prompt for this session, seeded before it existed ──
        let state_a = state.clone();
        let session_key_a = session_key.clone();
        let turn_a_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut sender = CollectSink(Vec::new());
            let mut receiver =
                futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
            let (_approval_tx, mut approval_rx) = tokio::sync::mpsc::channel(4);
            let pending = new_pending_approvals();

            assert!(
                !state_a
                    .cancel_tokens
                    .lock()
                    .unwrap()
                    .contains_key(&session_key_a),
                "turn A is the first prompt for this session"
            );
            let _guard = state_a
                .session_queue
                .acquire(&session_key_a)
                .await
                .expect("A acquires the session lock uncontested");

            process_chat_message(
                &state_a,
                &mut agent_a,
                &mut sender,
                &mut receiver,
                &mut approval_rx,
                &pending,
                &None,
                "prompt-A",
                &session_key_a,
                "shared-session",
                None,
            )
            .await;

            (agent_a, sender)
        });

        // Wait for A to register as live: its `chat_with_system` call is now
        // parked mid-turn (it never resolves on its own). This spin is only
        // ever used to make the *benign* ordering deterministic (B arrives
        // strictly after A is live) — the racy-interleaving test below
        // deliberately does not use anything like it.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !state
            .cancel_tokens
            .lock()
            .unwrap()
            .contains_key(&session_key)
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "turn A never registered as live"
            );
            tokio::task::yield_now().await;
        }
        let token_during_a = state
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .get(&session_key)
            .cloned()
            .expect("A's cancel token is registered while A is live");
        assert!(!token_during_a.is_cancelled(), "A has not been aborted yet");

        // ── Prompt B arrives while A is live ──
        let state_b = state.clone();
        let session_key_b = session_key.clone();
        let turn_b_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut sender = CollectSink(Vec::new());
            let mut receiver =
                futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
            let (_approval_tx, mut approval_rx) = tokio::sync::mpsc::channel(4);
            let pending = new_pending_approvals();

            // Connect-time snapshot: this connection has never seen a turn
            // complete for this session.
            let seen_version_b =
                current_turn_version(&state_b.session_turn_versions, &session_key_b);

            let _guard = state_b
                .session_queue
                .acquire(&session_key_b)
                .await
                .expect("B eventually acquires the lock once A releases it");

            let rehydrated = current_turn_version(&state_b.session_turn_versions, &session_key_b)
                != seen_version_b;
            if rehydrated && let Some(ref backend) = state_b.session_backend {
                rehydrate_agent_from_backend(backend.as_ref(), &mut agent_b, &session_key_b);
            }

            process_chat_message(
                &state_b,
                &mut agent_b,
                &mut sender,
                &mut receiver,
                &mut approval_rx,
                &pending,
                &None,
                "prompt-B",
                &session_key_b,
                "shared-session",
                None,
            )
            .await;

            (agent_b, sender, rehydrated)
        });

        // ── Property 1: B is queued/attached behind A, never run concurrently ──
        // `queue_depth` counts every live `acquire` attempt, including the one
        // currently holding the permit: 1 == only A (holding); 2 == A holding
        // + B waiting behind it. It can never observe B running concurrently
        // with A, since B's own `acquire` has not yet resolved.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.session_queue.queue_depth(&session_key).await < 2 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "B never reached the session queue"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(
            state.session_queue.queue_depth(&session_key).await,
            2,
            "B must be queued behind A (A holding + B waiting), not running concurrently with it"
        );
        assert_eq!(
            state.cancel_tokens.lock().unwrap().len(),
            1,
            "only A's turn may hold registered abort authority while B is queued"
        );

        // ── Property 3: B cannot displace A's abort authority ──
        // B is still queued and has registered no token of its own, so an
        // abort issued right now can only ever land on A's token — there is
        // categorically nothing for B to have displaced.
        let abort_response = crate::api::handle_api_session_abort(
            State(state.clone()),
            HeaderMap::new(),
            axum::extract::Path(session_id.to_string()),
        )
        .await
        .into_response();
        let abort_json = queue_test_response_json(abort_response).await;
        assert_eq!(abort_json["status"], "aborted");
        assert!(
            token_during_a.is_cancelled(),
            "the abort must cancel the exact token instance A registered"
        );

        // A's turn now unwinds via cancellation and releases the session
        // permit; B's queued acquire can proceed.
        let (_agent_a, _sender_a) = turn_a_handle.await.expect("turn A task does not panic");
        let (agent_b, sender_b, rehydrated) =
            turn_b_handle.await.expect("turn B task does not panic");
        assert!(
            rehydrated,
            "B must have rehydrated after observing a turn complete while it waited"
        );

        // ── Property 2: B never runs from the pre-A history it was seeded
        // with at connect time ──
        assert_b_rehydrated_after_a_aborted(&state, &backend, &session_key, &agent_b, &sender_b);
    }

    /// The gateway shutdown watch channel
    /// (`AppState::shutdown_tx`, observed by `run_gateway`'s accept loop) was
    /// not observed anywhere inside a live turn, so a detached turn (viewer
    /// already gone) would keep running provider/tool calls straight through
    /// a graceful shutdown. Shutdown must cancel a live turn the same way an
    /// explicit abort does, so it unwinds, persists partial output, and
    /// releases its token/permit instead of outliving the process trying to
    /// stop it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_cancels_a_detached_turn() {
        let state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        let session_id = "shutdown-detached-turn";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

        let mut agent = queue_test_agent(Box::new(StuckModelProvider));

        let state_for_turn = state.clone();
        let session_key_for_turn = session_key.clone();
        let turn_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut sender = CollectSink(Vec::new());
            // Already-empty stream: the very first `receiver.next()` poll
            // resolves to `None`, exactly like a viewer that disconnected
            // before this turn even started — the detached-turn case
            // shutdown cancellation has to cover.
            let mut receiver =
                futures_util::stream::empty::<Result<Message, std::convert::Infallible>>();
            let (_approval_tx, mut approval_rx) = tokio::sync::mpsc::channel(4);
            let pending = new_pending_approvals();

            process_chat_message(
                &state_for_turn,
                &mut agent,
                &mut sender,
                &mut receiver,
                &mut approval_rx,
                &pending,
                &None,
                "prompt-detached-then-shutdown",
                &session_key_for_turn,
                "shutdown-detached-turn",
                None,
            )
            .await;

            sender
        });

        // Wait for the turn to register as live before signalling shutdown.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !state
            .cancel_tokens
            .lock()
            .unwrap()
            .contains_key(&session_key)
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "turn never registered as live"
            );
            tokio::task::yield_now().await;
        }

        state
            .shutdown_tx
            .send(true)
            .expect("the live turn's own subscribe() keeps at least one receiver alive");

        let sender = tokio::time::timeout(std::time::Duration::from_secs(5), turn_handle)
            .await
            .expect(
                "the detached turn must end once shutdown is observed, not hang forever \
                 waiting on a provider call nobody will ever answer",
            )
            .expect("turn task does not panic");

        assert!(
            state
                .cancel_tokens
                .lock()
                .unwrap()
                .get(&session_key)
                .is_none(),
            "the cancel token must be released once shutdown-triggered cancellation unwinds"
        );
        let aborted = sender.0.iter().any(|frame| {
            serde_json::from_str::<serde_json::Value>(frame)
                .ok()
                .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
                .as_deref()
                == Some("aborted")
        });
        assert!(
            aborted,
            "shutdown must cancel the turn the same way an explicit abort does: {:?}",
            sender.0
        );
    }

    /// `DELETE /api/sessions/{id}` cancels the
    /// *currently registered* live token, but a prompt already queued behind
    /// that turn on `session_queue` has not registered a token of its own
    /// yet. Without an explicit check, that queued prompt is free to acquire
    /// the permit A just released and start provider/tool execution for a
    /// session an operator just deleted. `session_deleted_while_queued` —
    /// the exact guard `handle_socket` calls between acquiring the permit
    /// and calling `process_chat_message` — must observe the deletion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn no_queued_prompt_starts_after_delete() {
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        let backend = std::sync::Arc::new(QueueTestSessionBackend::default());
        state.session_backend = Some(backend.clone());
        let state = state;

        let session_id = "deleted-while-queued";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

        let mut agent_a = queue_test_agent(Box::new(StuckModelProvider));

        // ── Turn A: live, holding the session_queue permit ──
        let state_a = state.clone();
        let session_key_a = session_key.clone();
        let turn_a_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut sender = CollectSink(Vec::new());
            let mut receiver =
                futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
            let (_approval_tx, mut approval_rx) = tokio::sync::mpsc::channel(4);
            let pending = new_pending_approvals();
            let _guard = state_a
                .session_queue
                .acquire(&session_key_a)
                .await
                .expect("A acquires the session lock uncontested");

            process_chat_message(
                &state_a,
                &mut agent_a,
                &mut sender,
                &mut receiver,
                &mut approval_rx,
                &pending,
                &None,
                "prompt-A",
                &session_key_a,
                "deleted-while-queued",
                None,
            )
            .await;

            (agent_a, sender)
        });

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !state
            .cancel_tokens
            .lock()
            .unwrap()
            .contains_key(&session_key)
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "turn A never registered as live"
            );
            tokio::task::yield_now().await;
        }

        // ── Prompt B queues behind A, then checks the same guard
        // `handle_socket` runs right after its own `acquire()` resolves ──
        let state_b = state.clone();
        let session_key_b = session_key.clone();
        // Captured before B starts waiting, matching `handle_socket`.
        let generation_b = state.session_lifecycle.deletion_generation(&session_key);
        let b_handle = ::zeroclaw_spawn::spawn!(async move {
            let _guard = state_b
                .session_queue
                .acquire(&session_key_b)
                .await
                .expect("B eventually acquires the lock once A releases it");
            session_deleted_while_queued(&state_b, &session_key_b, generation_b)
        });

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.session_queue.queue_depth(&session_key).await < 2 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "B never reached the session queue"
            );
            tokio::task::yield_now().await;
        }

        // ── Delete the session while A is live and B is queued behind it ──
        let delete_response = crate::api::handle_api_session_delete(
            State(state.clone()),
            HeaderMap::new(),
            axum::extract::Path(session_id.to_string()),
        )
        .await
        .into_response();
        let delete_json = queue_test_response_json(delete_response).await;
        assert_eq!(delete_json["deleted"], true);

        // A's turn now unwinds via the delete-triggered cancellation and
        // releases the permit; B's queued acquire can proceed.
        let (_agent_a, _sender_a) = turn_a_handle.await.expect("turn A task does not panic");
        let b_saw_deleted = b_handle.await.expect("B's task does not panic");

        assert!(
            b_saw_deleted,
            "a prompt queued behind a deleted session's turn must observe the \
             deletion via session_deleted_while_queued before handle_socket \
             would otherwise call process_chat_message for it"
        );
        assert!(
            backend.load(&session_key).is_empty(),
            "no queued prompt may have appended to the deleted session"
        );
    }

    /// Racy interleaving: B's decision to wait behind A is forced by queue
    /// position alone — B enqueues on `session_queue` *before A has
    /// registered any cancel token at all* (A is still queued too, behind a
    /// permit the test primes and holds). This is the TOCTOU the fix
    /// removes: the old design snapshotted `cancel_tokens` for liveness
    /// *before* calling `acquire`, so a snapshot taken in this exact window
    /// (A enqueued but not yet registered) would read "not live" and skip
    /// rehydration even though A wins the permit and completes before B
    /// does. The fix makes the rehydrate decision strictly *after*
    /// `acquire` resolves, comparing a monotonic per-session version — a
    /// point that cannot be reached until the connection actually holds the
    /// permit, by which time any prior turn for this session is
    /// unconditionally finished (see the version bump next to the
    /// cancel-token removal in `process_chat_message`). So this test
    /// forces B to enqueue with zero information about A's state, and
    /// still expects the same three properties.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_prompt_still_rehydrates_from_post_turn_a_history_and_keeps_a_abort_authority() {
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        let backend = std::sync::Arc::new(QueueTestSessionBackend::default());
        state.session_backend = Some(backend.clone());
        let state = state;

        let session_id = "raced-session";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

        let mut agent_a = queue_test_agent(Box::new(StuckModelProvider));
        let mut agent_b = queue_test_agent(Box::new(ImmediateModelProvider("B-reply")));

        // Prime the session's one permit so neither A nor B can win the
        // `acquire` race until the test releases it below. `Semaphore` is
        // documented FIFO, so this lets the test control *queue order*
        // deterministically (A enqueues first, B second) without ever
        // consulting `cancel_tokens` — the one piece of state the old,
        // buggy design leaned on and that this test must not depend on.
        let priming_guard = state
            .session_queue
            .acquire(&session_key)
            .await
            .expect("test primes the session permit");

        let state_a = state.clone();
        let session_key_a = session_key.clone();
        let turn_a_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut sender = CollectSink(Vec::new());
            let mut receiver =
                futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
            let (_approval_tx, mut approval_rx) = tokio::sync::mpsc::channel(4);
            let pending = new_pending_approvals();

            // No pre-acquire check of any kind — the fix removed the last
            // one (`session_turn_is_live`). A just enqueues.
            let _guard = state_a
                .session_queue
                .acquire(&session_key_a)
                .await
                .expect("A eventually wins the primed permit");

            process_chat_message(
                &state_a,
                &mut agent_a,
                &mut sender,
                &mut receiver,
                &mut approval_rx,
                &pending,
                &None,
                "prompt-A",
                &session_key_a,
                "raced-session",
                None,
            )
            .await;

            (agent_a, sender)
        });

        // Wait for A to actually enqueue behind the primed permit (2 ==
        // priming guard holding + A waiting). Purely a `session_queue`
        // accounting check — nothing here touches `cancel_tokens`.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.session_queue.queue_depth(&session_key).await < 2 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "A never enqueued behind the primed permit"
            );
            tokio::task::yield_now().await;
        }

        // Spawn B *immediately* — no wait for A's cancel token, because
        // there isn't one: A is still queued behind the priming guard,
        // nowhere near `register_cancel_token`. This is the racy
        // interleaving: B's `acquire` call is issued while A has registered
        // nothing at all.
        let state_b = state.clone();
        let session_key_b = session_key.clone();
        let turn_b_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut sender = CollectSink(Vec::new());
            let mut receiver =
                futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
            let (_approval_tx, mut approval_rx) = tokio::sync::mpsc::channel(4);
            let pending = new_pending_approvals();

            let seen_version_b =
                current_turn_version(&state_b.session_turn_versions, &session_key_b);

            let _guard = state_b
                .session_queue
                .acquire(&session_key_b)
                .await
                .expect("B eventually wins the permit after A");

            let rehydrated = current_turn_version(&state_b.session_turn_versions, &session_key_b)
                != seen_version_b;
            if rehydrated && let Some(ref backend) = state_b.session_backend {
                rehydrate_agent_from_backend(backend.as_ref(), &mut agent_b, &session_key_b);
            }

            process_chat_message(
                &state_b,
                &mut agent_b,
                &mut sender,
                &mut receiver,
                &mut approval_rx,
                &pending,
                &None,
                "prompt-B",
                &session_key_b,
                "raced-session",
                None,
            )
            .await;

            (agent_b, sender, rehydrated)
        });

        // Wait for both A and B to be enqueued behind the priming guard
        // (3 == priming holding + A waiting + B waiting) — B has issued its
        // `acquire` call while A is nowhere near registering a cancel
        // token (A hasn't even won the permit yet).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.session_queue.queue_depth(&session_key).await < 3 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "B never enqueued behind A"
            );
            tokio::task::yield_now().await;
        }
        assert!(
            state.cancel_tokens.lock().unwrap().is_empty(),
            "neither A nor B has registered a cancel token yet — both are still queued \
             behind the primed permit, which is exactly the window the old \
             pre-acquire liveness snapshot got wrong"
        );

        // Release the primed permit: FIFO means A (enqueued first) wins it,
        // not B — this is the only ordering guarantee the test relies on,
        // and it comes from `session_queue`'s documented fairness, not from
        // observing A's internal state.
        drop(priming_guard);

        // A wins the race and registers legitimately (a consequence of the
        // race resolving, not something the test orchestrates).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.cancel_tokens.lock().unwrap().is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "A never won the primed permit and registered"
            );
            tokio::task::yield_now().await;
        }
        let token_during_a = state
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .get(&session_key)
            .cloned()
            .expect("A's cancel token is registered once A wins the permit");
        assert!(!token_during_a.is_cancelled(), "A has not been aborted yet");

        // ── Property 1: B is queued/attached behind A, never run concurrently ──
        assert_eq!(
            state.session_queue.queue_depth(&session_key).await,
            2,
            "B must be queued behind A (A holding + B waiting), not running concurrently with it"
        );
        assert_eq!(
            state.cancel_tokens.lock().unwrap().len(),
            1,
            "only A's turn may hold registered abort authority while B is queued"
        );

        // ── Property 3: B cannot displace A's abort authority ──
        let abort_response = crate::api::handle_api_session_abort(
            State(state.clone()),
            HeaderMap::new(),
            axum::extract::Path(session_id.to_string()),
        )
        .await
        .into_response();
        let abort_json = queue_test_response_json(abort_response).await;
        assert_eq!(abort_json["status"], "aborted");
        assert!(
            token_during_a.is_cancelled(),
            "the abort must cancel the exact token instance A registered"
        );

        let (_agent_a, _sender_a) = turn_a_handle.await.expect("turn A task does not panic");
        let (agent_b, sender_b, rehydrated) =
            turn_b_handle.await.expect("turn B task does not panic");
        assert!(
            rehydrated,
            "B must rehydrate even though its own pre-acquire state (none) said nothing \
             about A — the version check under the permit must catch what a pre-acquire \
             liveness snapshot would have missed"
        );

        // ── Property 2: B never runs from the pre-A history it was seeded
        // with at connect time ──
        assert_b_rehydrated_after_a_aborted(&state, &backend, &session_key, &agent_b, &sender_b);
    }

    /// Connect-time variant of the same TOCTOU: a *new* connection's
    /// connect-time snapshot (`seen_version` read, then `backend.load` —
    /// mirrored here exactly as `handle_socket` does it, ws.rs:~345/~351)
    /// happens without holding the `session_queue` permit, so it can race a
    /// concurrently-completing turn. The bug this closes: bumping the
    /// version *before* persisting would let this snapshot observe the new
    /// version while the messages behind it are not yet in the backend —
    /// then load stale (pre-turn) history and seed an `Agent` that looks
    /// exactly as current as a correctly-rehydrated one, permanently.
    ///
    /// The fix makes that ordering structurally impossible rather than just
    /// less likely: `bump_turn_version` runs strictly after that path's
    /// persistence, on the same synchronous call path with no `.await`
    /// between them (see the three call sites in `process_chat_message`),
    /// so any observer's happens-before chain through the `session_backend`
    /// lock and the `session_turn_versions` lock can only ever see
    /// messages-before-version, never the reverse. There is no `.await`
    /// point between persist and bump to pin a test to without
    /// instrumenting production code (out of scope here), so this test
    /// does not force the exact instruction-level interleaving; instead,
    /// per the documented fallback for that case, it:
    /// (1) runs an independent sampler that polls (version, backend
    ///     content) many times on a real multi-thread runtime throughout
    ///     turn A's completion and asserts the dangerous order — version
    ///     bumped, A's message not yet in the backend — is never observed;
    /// (2) has a connection C perform the exact connect-time two-step
    ///     concurrently with A's completion, with nothing pinning it to any
    ///     particular instant, and proves C's first prompt still lands on
    ///     A's fully-persisted history regardless of what that snapshot
    ///     caught — the post-acquire version compare is the safety net for
    ///     whatever the connect-time snapshot missed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn connect_time_snapshot_never_outraces_persistence_before_version_bump() {
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        let backend = std::sync::Arc::new(QueueTestSessionBackend::default());
        state.session_backend = Some(backend.clone());
        let state = state;

        let session_id = "connect-race-session";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

        let mut agent_a = queue_test_agent(Box::new(StuckModelProvider));

        // ── Turn A: registers, then sits parked mid-turn until aborted. ──
        let state_a = state.clone();
        let session_key_a = session_key.clone();
        let turn_a_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut sender = CollectSink(Vec::new());
            let mut receiver =
                futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
            let (_approval_tx, mut approval_rx) = tokio::sync::mpsc::channel(4);
            let pending = new_pending_approvals();
            let _guard = state_a
                .session_queue
                .acquire(&session_key_a)
                .await
                .expect("A acquires the session lock uncontested");
            process_chat_message(
                &state_a,
                &mut agent_a,
                &mut sender,
                &mut receiver,
                &mut approval_rx,
                &pending,
                &None,
                "prompt-A",
                &session_key_a,
                "connect-race-session",
                None,
            )
            .await;
            (agent_a, sender)
        });

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !state
            .cancel_tokens
            .lock()
            .unwrap()
            .contains_key(&session_key)
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "turn A never registered as live"
            );
            tokio::task::yield_now().await;
        }

        // ── (1) Independent sampler: polls (version, backend content) many
        // times across A's completion and keeps every distinct pair. ──
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sampler_state = state.clone();
        let sampler_session_key = session_key.clone();
        let sampler_backend = backend.clone();
        let sampler_stop = stop.clone();
        let sampler_handle = ::zeroclaw_spawn::spawn!(async move {
            let mut samples: Vec<(u64, bool)> = Vec::new();
            let sampler_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            // Not an unconditional spin: stops a little after the caller
            // signals turn A is done (or the deadline trips), so samples
            // span before/during/after completion without running forever.
            let mut extra_after_stop = 50;
            loop {
                let version = current_turn_version(
                    &sampler_state.session_turn_versions,
                    &sampler_session_key,
                );
                let has_prompt_a = sampler_backend
                    .load(&sampler_session_key)
                    .iter()
                    .any(|m| m.content.contains("prompt-A"));
                samples.push((version, has_prompt_a));
                if sampler_stop.load(std::sync::atomic::Ordering::Acquire) {
                    extra_after_stop -= 1;
                    if extra_after_stop == 0 {
                        break;
                    }
                }
                if tokio::time::Instant::now() >= sampler_deadline {
                    break;
                }
                tokio::task::yield_now().await;
            }
            samples
        });

        // ── (2) Connection C performs the exact connect-time two-step,
        // concurrently with A's completion, with nothing pinning it to any
        // particular instant relative to A's persist/bump. ──
        let state_c = state.clone();
        let session_key_c = session_key.clone();
        let connect_handle = ::zeroclaw_spawn::spawn!(async move {
            // Mirrors ws.rs:~345 / ~351 exactly: read the version, *then*
            // load history — this connection holds no permit at this point.
            let seen_version_c =
                current_turn_version(&state_c.session_turn_versions, &session_key_c);
            let stored_messages_c = state_c
                .session_backend
                .as_ref()
                .expect("backend configured")
                .load(&session_key_c);
            (seen_version_c, stored_messages_c)
        });

        // Abort A while the sampler and C's connect-time snapshot are
        // racing its completion.
        let abort_response = crate::api::handle_api_session_abort(
            State(state.clone()),
            HeaderMap::new(),
            axum::extract::Path(session_id.to_string()),
        )
        .await
        .into_response();
        let abort_json = queue_test_response_json(abort_response).await;
        assert_eq!(abort_json["status"], "aborted");

        let (_agent_a, _sender_a) = turn_a_handle.await.expect("turn A task does not panic");
        let (seen_version_c, stored_messages_c) = connect_handle
            .await
            .expect("C's connect-time snapshot task does not panic");

        stop.store(true, std::sync::atomic::Ordering::Release);
        let samples = sampler_handle.await.expect("sampler task does not panic");

        // ── Assertion (1): the dangerous ordering is never observed —
        // version bumped without A's message yet visible in the backend.
        // Fold C's own snapshot in too: it is just one more
        // (version, backend-content) pair, taken at a real, uncontrolled
        // instant during A's completion. ──
        let stored_c_has_prompt_a = stored_messages_c
            .iter()
            .any(|m| m.content.contains("prompt-A"));
        let all_samples: Vec<(u64, bool)> = samples
            .iter()
            .copied()
            .chain(std::iter::once((seen_version_c, stored_c_has_prompt_a)))
            .collect();
        let violations: Vec<&(u64, bool)> = all_samples
            .iter()
            .filter(|&&(version, has_prompt_a)| version > 0 && !has_prompt_a)
            .collect();
        assert!(
            violations.is_empty(),
            "observed the session's turn-completion version bumped before A's \
             message was visible in the backend: {violations:?} (out of {} samples)",
            all_samples.len()
        );
        assert!(
            all_samples.iter().any(|&(v, _)| v > 0),
            "the sampler never observed the post-bump state at all in this run — \
             widen its window; this run did not actually exercise the invariant"
        );

        // ── Assertion (2): C's own first prompt still lands on A's complete
        // history, regardless of what its connect-time snapshot caught. ──
        let mut agent_c = queue_test_agent(Box::new(ImmediateModelProvider("C-reply")));
        if !stored_messages_c.is_empty() {
            agent_c.seed_history_with_event(&stored_messages_c);
        }
        let mut sender_c = CollectSink(Vec::new());
        let mut receiver_c =
            futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
        let (_approval_tx, mut approval_rx_c) = tokio::sync::mpsc::channel(4);
        let pending_c = new_pending_approvals();
        let _guard_c = state
            .session_queue
            .acquire(&session_key)
            .await
            .expect("C acquires the session lock once A has fully released it");
        if current_turn_version(&state.session_turn_versions, &session_key) != seen_version_c
            && let Some(ref backend_ref) = state.session_backend
        {
            rehydrate_agent_from_backend(backend_ref.as_ref(), &mut agent_c, &session_key);
        }
        process_chat_message(
            &state,
            &mut agent_c,
            &mut sender_c,
            &mut receiver_c,
            &mut approval_rx_c,
            &pending_c,
            &None,
            "prompt-C",
            &session_key,
            "connect-race-session",
            None,
        )
        .await;

        let agent_c_saw_prompt_a = agent_c.history().iter().any(|m| {
            matches!(
                m,
                zeroclaw_providers::ConversationMessage::Chat(c) if c.content.contains("prompt-A")
            )
        });
        assert!(
            agent_c_saw_prompt_a,
            "C's Agent must reflect turn A's completed history before C's own \
             prompt runs, regardless of what C's connect-time snapshot caught: {:?}",
            agent_c.history()
        );
    }

    /// A brand-new JSONL-backed session must be able to run its first prompt.
    ///
    /// `SessionStore::session_exists` is a file-presence check and the session
    /// file is not created until the first append, so a guard built on that
    /// probe reports "absent" for a session that was never deleted — rejecting
    /// the opening prompt of every new session as `SESSION_DELETED`. The
    /// production JSONL backend is used deliberately: a custom test backend
    /// that pre-creates its sessions cannot express this state.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn first_prompt_on_a_new_jsonl_session_is_not_treated_as_deleted() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let store = std::sync::Arc::new(
            zeroclaw_infra::session_store::SessionStore::new(tmp.path()).expect("session store"),
        );
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.session_backend = Some(store.clone());
        let state = state;

        let session_key = format!("{GW_SESSION_PREFIX}brand-new-session");

        // Mirror the handshake: the alias write is a no-op for this backend,
        // so no file exists yet.
        let _ = store.set_session_agent_alias(&session_key, "default");
        assert!(
            !store.session_exists(&session_key),
            "precondition: a new JSONL session has no file until its first append; \
             if this ever becomes false the bug this test guards is unreachable \
             and the test is no longer meaningful"
        );

        // Exactly what `handle_socket` does: capture before waiting, compare
        // after acquiring.
        let generation = state.session_lifecycle.deletion_generation(&session_key);
        let _guard = state
            .session_queue
            .acquire(&session_key)
            .await
            .expect("uncontended acquire");

        assert!(
            !session_deleted_while_queued(&state, &session_key, generation),
            "a session that was never deleted must not be reported as deleted just \
             because its backing file has not been written yet"
        );
    }

    /// A queued writer must still observe a deletion when the session is
    /// recreated before it wakes: existence is restored, but the writer's
    /// view of history is stale either way.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_then_recreate_still_rejects_the_queued_prompt() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let store = std::sync::Arc::new(
            zeroclaw_infra::session_store::SessionStore::new(tmp.path()).expect("session store"),
        );
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.session_backend = Some(store.clone());
        let state = state;

        let session_id = "recycled-session";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");
        store
            .append(&session_key, &zeroclaw_providers::ChatMessage::user("seed"))
            .expect("seed append");

        let generation = state.session_lifecycle.deletion_generation(&session_key);

        let delete_response = crate::api::handle_api_session_delete(
            State(state.clone()),
            HeaderMap::new(),
            axum::extract::Path(session_id.to_string()),
        )
        .await
        .into_response();
        assert_eq!(
            queue_test_response_json(delete_response).await["deleted"],
            true
        );

        // Recreate it, so a file-presence probe would report the session as
        // perfectly healthy.
        store
            .append(
                &session_key,
                &zeroclaw_providers::ChatMessage::user("recreated"),
            )
            .expect("recreate append");
        assert!(store.session_exists(&session_key));

        assert!(
            session_deleted_while_queued(&state, &session_key, generation),
            "delete-then-recreate must invalidate a writer that queued before the \
             delete, even though the session exists again"
        );
    }

    /// The session must not read as `idle` between the end of streaming and
    /// the completion of persistence: that is precisely the window a
    /// reconnecting dashboard uses to hydrate its transcript.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_state_reports_running_while_a_turn_is_finalizing() {
        let state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        let session_id = "finalizing-session";
        let session_key = format!("{GW_SESSION_PREFIX}{session_id}");

        // No cancel token is registered — the stream has ended. Only the
        // finalizing hold stands between the client and a premature `idle`.
        let finalizing = crate::session_lifecycle::FinalizingGuard::new(
            state.session_lifecycle.as_ref(),
            &session_key,
        );

        let response = crate::api::handle_api_session_state(
            State(state.clone()),
            HeaderMap::new(),
            axum::extract::Path(session_id.to_string()),
        )
        .await
        .into_response();
        let json = queue_test_response_json(response).await;
        assert_eq!(
            json["state"], "running",
            "a turn that has stopped streaming but not finished persisting must not \
             be advertised as idle: {json:?}"
        );

        drop(finalizing);

        let response = crate::api::handle_api_session_state(
            State(state.clone()),
            HeaderMap::new(),
            axum::extract::Path(session_id.to_string()),
        )
        .await
        .into_response();
        let json = queue_test_response_json(response).await;
        assert_eq!(
            json["state"], "idle",
            "once persistence settles the session must become idle again: {json:?}"
        );
    }
}
