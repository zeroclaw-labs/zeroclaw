//! ACP-over-WebSocket gateway endpoint.

use super::AppState;
use crate::dashboard_session::{
    DashboardAuth, DashboardCaller, SessionWatch, WS_CLOSE_SESSION_ENDED, session_lost,
};
use axum::{
    Extension,
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::HeaderMap,
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::jsonrpc::error_codes::{FORBIDDEN, SESSION_BUSY};
use zeroclaw_channels::orchestrator::acp_server::{AcpServer, AcpServerConfig};
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::session_queue::{SessionActorQueue, SessionGuard, SessionQueueError};

const ACP_WS_PROTOCOL: &str = "zeroclaw.acp.v1";
const MAX_BUFFERED_TURN_EVENTS: usize = 4096;

/// One sequenced frame emitted by an in-flight ACP turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpTurnEventFrame {
    pub(crate) seq: u64,
    pub(crate) payload: String,
    pub(crate) is_terminal: bool,
}

struct ActiveAcpTurn {
    principal_id: Option<String>,
    events: Vec<AcpTurnEventFrame>,
    next_seq: u64,
    completed: bool,
    tx: broadcast::Sender<AcpTurnEventFrame>,
    cancel: CancellationToken,
    _guard: SessionGuard,
}

/// Snapshot + live receiver returned when a reconnected socket reattaches to
/// an in-flight ACP turn.
pub(crate) struct AcpTurnAttachment {
    pub(crate) replay: Vec<AcpTurnEventFrame>,
    pub(crate) live: broadcast::Receiver<AcpTurnEventFrame>,
    pub(crate) completed: bool,
}

/// Gateway-shared registry of in-flight ACP turns, backed by [`SessionActorQueue`]
/// for per-session serialization and bounded in-memory event replay on reload.
pub(crate) struct AcpTurnHub {
    queue: Arc<SessionActorQueue>,
    turns: StdMutex<HashMap<String, ActiveAcpTurn>>,
}

impl AcpTurnHub {
    pub(crate) fn new(queue: Arc<SessionActorQueue>) -> Self {
        Self {
            queue,
            turns: StdMutex::new(HashMap::new()),
        }
    }

    /// Return the canonical [`AcpTurnHub`] associated with a gateway's
    /// [`SessionActorQueue`], automatically pruning entries whose queue has
    /// been dropped.
    pub(crate) fn for_queue(queue: &Arc<SessionActorQueue>) -> Arc<Self> {
        type AcpTurnHubMap = HashMap<usize, (Weak<SessionActorQueue>, Arc<AcpTurnHub>)>;
        static HUBS: OnceLock<StdMutex<AcpTurnHubMap>> = OnceLock::new();
        let map = HUBS.get_or_init(|| StdMutex::new(HashMap::new()));
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|_, (weak, _)| weak.strong_count() > 0);
        let key = Arc::as_ptr(queue) as usize;
        if let Some((_, existing)) = guard.get(&key) {
            return Arc::clone(existing);
        }
        let hub = Arc::new(Self::new(Arc::clone(queue)));
        guard.insert(key, (Arc::downgrade(queue), Arc::clone(&hub)));
        hub
    }

    /// Acquire the per-session [`SessionActorQueue`] permit and register an
    /// active turn for `session_id`.
    pub(crate) async fn start_turn(
        &self,
        session_id: &str,
        principal_id: Option<String>,
        cancel: CancellationToken,
    ) -> Result<(), SessionQueueError> {
        let permit = self.queue.acquire(session_id).await?;
        let (tx, _) = broadcast::channel(256);
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        turns.insert(
            session_id.to_string(),
            ActiveAcpTurn {
                principal_id,
                events: Vec::new(),
                next_seq: 1,
                completed: false,
                tx,
                cancel,
                _guard: permit,
            },
        );
        Ok(())
    }

    /// Append a notification or terminal frame to an active turn and fan it
    /// out to any reattached sockets in exact sequence order.
    pub(crate) fn record_frame(
        &self,
        session_id: &str,
        payload: String,
        is_terminal: bool,
    ) -> Option<u64> {
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let turn = turns.get_mut(session_id)?;
        let seq = turn.next_seq;
        turn.next_seq = turn.next_seq.saturating_add(1);
        let frame = AcpTurnEventFrame {
            seq,
            payload,
            is_terminal,
        };
        if turn.events.len() < MAX_BUFFERED_TURN_EVENTS {
            turn.events.push(frame.clone());
        }
        if is_terminal {
            turn.completed = true;
        }
        let _ = turn.tx.send(frame);
        Some(seq)
    }

    /// Finish and remove an active turn, releasing its [`SessionGuard`] permit.
    pub(crate) fn finish_turn(&self, session_id: &str) {
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        turns.remove(session_id);
    }

    /// Reattach to an active turn for `session_id`, enforcing exact principal
    /// ownership. Returns `Ok(None)` when no turn is currently in flight,
    /// `Err(FORBIDDEN)` when owned by another principal, and `Ok(Some(_))` with
    /// ordered replay events (`seq > after_seq`) plus a live broadcast stream.
    pub(crate) fn reattach(
        &self,
        session_id: &str,
        principal_id: Option<&str>,
        after_seq: u64,
    ) -> Result<Option<AcpTurnAttachment>, i32> {
        let turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(turn) = turns.get(session_id) else {
            return Ok(None);
        };
        if turn.principal_id.as_deref() != principal_id {
            return Err(FORBIDDEN);
        }
        let replay = turn
            .events
            .iter()
            .filter(|event| event.seq > after_seq)
            .cloned()
            .collect();
        Ok(Some(AcpTurnAttachment {
            replay,
            live: turn.tx.subscribe(),
            completed: turn.completed,
        }))
    }

    /// Cancel an active turn if owned by `principal_id`.
    pub(crate) fn cancel_turn(
        &self,
        session_id: &str,
        principal_id: Option<&str>,
    ) -> Result<bool, i32> {
        let turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(turn) = turns.get(session_id) else {
            return Ok(false);
        };
        if turn.principal_id.as_deref() != principal_id {
            return Err(FORBIDDEN);
        }
        turn.cancel.cancel();
        Ok(true)
    }
}

#[derive(Debug, Deserialize)]
pub struct AcpQuery {
    token: Option<String>,
    /// Connection-scoped default agent alias. Used when `session/new` omits
    /// `agentAlias`, so spec-vanilla one-agent-per-endpoint ACP clients can
    /// address each configured agent via its own URL. Validated exactly like
    /// an explicit `agentAlias` at `session/new`; ignored by session restore.
    agent: Option<String>,
}

pub async fn handle_ws_acp(
    State(state): State<AppState>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    Query(params): Query<AcpQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let mut session_watch = None;
    let mut principal_id = None;

    if state.pairing.require_pairing() {
        if let Some(token) = extract_ws_token(&headers, params.token.as_deref()) {
            if !state.pairing.is_authenticated(token) {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    "Unauthorized - provide Authorization header, Sec-WebSocket-Protocol bearer, or ?token= query param",
                )
                    .into_response();
            }
        } else if let Some(caller) = crate::dashboard_session::current_caller() {
            match (caller, dashboard) {
                (DashboardCaller::Session(session), Some(Extension(auth))) => {
                    let is_admin = session.is_admin();
                    if is_admin {
                        session_watch = Some(SessionWatch::new(auth, session, true, &state));
                    } else {
                        let pid = session.principal().id.as_str().to_string();
                        let watch = if let Some(ref agent) = params.agent {
                            SessionWatch::for_chat(auth, session, agent, &state)
                        } else {
                            SessionWatch::new(auth, session, false, &state)
                        };
                        session_watch = Some(watch);
                        principal_id = Some(pid);
                    }
                }
                (DashboardCaller::Rejected(_), _) => {
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        "Unauthorized - dashboard session is no longer valid",
                    )
                        .into_response();
                }
                _ => {
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        "Unauthorized - provide Authorization header, Sec-WebSocket-Protocol bearer, or ?token= query param",
                    )
                        .into_response();
                }
            }
        } else {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized - provide Authorization header, Sec-WebSocket-Protocol bearer, or ?token= query param",
            )
                .into_response();
        }
    }

    let ws = if headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == ACP_WS_PROTOCOL))
    {
        ws.protocols([ACP_WS_PROTOCOL])
    } else {
        ws
    };

    ws.on_upgrade(move |socket| {
        handle_socket(socket, state, params.agent, session_watch, principal_id)
    })
    .into_response()
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    default_agent: Option<String>,
    mut session_watch: Option<SessionWatch>,
    principal_id: Option<String>,
) {
    let (mut sender, mut receiver) = socket.split();
    let (input_tx, input_rx) = mpsc::channel::<String>(256);
    let (output_tx, mut output_rx) = mpsc::channel::<String>(256);
    let (wire_tx, mut wire_rx) = mpsc::channel::<String>(256);
    let turn_hub = AcpTurnHub::for_queue(&state.session_queue);
    let mut shutdown_rx = state.shutdown_tx.subscribe();

    let config = state.config.read().clone();
    let acp_config = AcpServerConfig {
        max_sessions: config.acp.max_sessions,
        session_timeout_secs: config.acp.session_timeout_secs,
    };
    let store = AcpSessionStore::new(&config.data_dir)
        .map(Arc::new)
        .inspect_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "Failed to open ACP session store"
            );
        })
        .ok();
    let canvas_store = state.canvas_store.clone();
    let server = if let Some(store) = store {
        Arc::new(
            AcpServer::new_with_live_config_and_writer_and_store(
                state.config.clone(),
                state.agent_lifecycle.clone(),
                acp_config,
                output_tx,
                store,
            )
            .with_canvas_store(canvas_store)
            .with_sop_engine(state.sop_engine.clone(), state.sop_audit.clone())
            .with_cost_tracker(state.cost_tracker.clone())
            .with_allow_webchat_policy(true)
            .with_principal_id(principal_id.clone())
            .with_connection_default_agent(default_agent),
        )
    } else {
        Arc::new(
            AcpServer::new_with_live_config_and_writer(
                state.config.clone(),
                state.agent_lifecycle.clone(),
                acp_config,
                output_tx,
            )
            .with_canvas_store(canvas_store)
            .with_sop_engine(state.sop_engine.clone(), state.sop_audit.clone())
            .with_cost_tracker(state.cost_tracker.clone())
            .with_allow_webchat_policy(true)
            .with_principal_id(principal_id.clone())
            .with_connection_default_agent(default_agent),
        )
    };

    let server_for_run = Arc::clone(&server);
    let server_task = zeroclaw_spawn::spawn!(server_for_run.run_messages(input_rx));

    // Track in-flight prompt request ids -> session ids on this connection so
    // outgoing `session/update` notifications and terminal prompt responses are
    // mirrored into `AcpTurnHub` for reload reattachment.
    let active_prompts: Arc<StdMutex<HashMap<String, String>>> =
        Arc::new(StdMutex::new(HashMap::new()));

    let hub_for_output = Arc::clone(&turn_hub);
    let prompts_for_output = Arc::clone(&active_prompts);
    let wire_tx_for_output = wire_tx.clone();
    let output_bridge = zeroclaw_spawn::spawn!(async move {
        while let Some(line) = output_rx.recv().await {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(method) = parsed.get("method").and_then(serde_json::Value::as_str)
                    && (method == "session/update" || method.starts_with("_zeroclaw/"))
                    && let Some(sid) = parsed
                        .get("params")
                        .and_then(|p| p.get("sessionId"))
                        .and_then(serde_json::Value::as_str)
                {
                    let _ = hub_for_output.record_frame(sid, line.clone(), false);
                } else if let Some(id) = parsed.get("id")
                    && (parsed.get("result").is_some() || parsed.get("error").is_some())
                {
                    let id_key = id.to_string();
                    let finished_sid = prompts_for_output
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id_key);
                    if let Some(sid) = finished_sid {
                        let _ = hub_for_output.record_frame(&sid, line.clone(), true);
                        hub_for_output.finish_turn(&sid);
                    }
                }
            }
            let _ = wire_tx_for_output.send(line).await;
        }
    });

    let close_signal = Arc::new(tokio::sync::Notify::new());
    let close_code: Arc<StdMutex<Option<(u16, &'static str)>>> = Arc::new(StdMutex::new(None));
    let close_signal_out = Arc::clone(&close_signal);
    let close_code_out = Arc::clone(&close_code);
    let output_task = zeroclaw_spawn::spawn!(async move {
        loop {
            tokio::select! {
                biased;
                () = close_signal_out.notified() => {
                    let frame = close_code_out
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take();
                    if let Some((code, reason)) = frame {
                        let _ = sender
                            .send(Message::Close(Some(CloseFrame {
                                code,
                                reason: reason.into(),
                            })))
                            .await;
                    }
                    break;
                }
                line = wire_rx.recv() => {
                    let Some(line) = line else { break; };
                    if sender.send(Message::Text(line.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut revoked = false;
    loop {
        tokio::select! {
            code = session_lost(&mut session_watch, &state) => {
                revoked = true;
                server.cancel_active_turns();
                *close_code.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((code, if code == WS_CLOSE_SESSION_ENDED { "dashboard session ended" } else { "access revoked" }));
                close_signal.notify_one();
                break;
            }
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    *close_code.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some((1012, "gateway shutting down"));
                    close_signal.notify_one();
                    break;
                }
            }
            maybe_msg = receiver.next() => {
                let Some(message) = maybe_msg else { break; };
                let text = match message {
                    Ok(Message::Text(text)) => text.to_string(),
                    Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                "ACP WebSocket received non-UTF-8 binary frame"
                            );
                            continue;
                        }
                    },
                    Ok(Message::Close(_)) => break,
                    Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("Connection reset without closing handshake")
                            || msg.contains("Connection closed normally")
                        {
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                                "ACP WebSocket closed without handshake"
                            );
                        } else {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                "ACP WebSocket receive error"
                            );
                        }
                        break;
                    }
                };

                // Intercept gateway-level turn coordination frames:
                // 1) `_zeroclaw/session/reattach` reattaches to an in-flight turn
                // 2) `session/cancel` cancels a detached turn if owned by this principal
                // 3) `session/prompt` acquires a `SessionActorQueue` permit & registers in `AcpTurnHub`
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text)
                    && let Some(method) = parsed.get("method").and_then(serde_json::Value::as_str)
                {
                    if method == "_zeroclaw/session/reattach" {
                        let req_id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        let sid = parsed
                            .get("params")
                            .and_then(|p| p.get("sessionId"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        let after_seq = parsed
                            .get("params")
                            .and_then(|p| p.get("afterSeq"))
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        match turn_hub.reattach(sid, principal_id.as_deref(), after_seq) {
                            Ok(Some(attachment)) => {
                                let ack = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": {
                                        "sessionId": sid,
                                        "inFlight": !attachment.completed,
                                        "replayed": attachment.replay.len(),
                                    }
                                });
                                let _ = wire_tx.send(ack.to_string()).await;
                                let wire_tx_sub = wire_tx.clone();
                                zeroclaw_spawn::spawn!(async move {
                                    let mut last_seq = after_seq;
                                    for ev in attachment.replay {
                                        last_seq = ev.seq;
                                        if wire_tx_sub.send(ev.payload).await.is_err() {
                                            return;
                                        }
                                        if ev.is_terminal {
                                            return;
                                        }
                                    }
                                    if attachment.completed {
                                        return;
                                    }
                                    let mut rx = attachment.live;
                                    while let Ok(ev) = rx.recv().await {
                                        if ev.seq <= last_seq {
                                            continue;
                                        }
                                        last_seq = ev.seq;
                                        let done = ev.is_terminal;
                                        if wire_tx_sub.send(ev.payload).await.is_err() || done {
                                            break;
                                        }
                                    }
                                });
                            }
                            Ok(None) => {
                                let ack = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": { "sessionId": sid, "inFlight": false, "replayed": 0 }
                                });
                                let _ = wire_tx.send(ack.to_string()).await;
                            }
                            Err(code) => {
                                let err = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "error": { "code": code, "message": "Session cannot be reattached" }
                                });
                                let _ = wire_tx.send(err.to_string()).await;
                            }
                        }
                        continue;
                    }
                    if method == "session/cancel"
                        && let Some(sid) = parsed
                            .get("params")
                            .and_then(|p| p.get("sessionId"))
                            .and_then(serde_json::Value::as_str)
                    {
                        let _ = turn_hub.cancel_turn(sid, principal_id.as_deref());
                    }
                    if method == "session/prompt"
                        && let Some(sid) = parsed
                            .get("params")
                            .and_then(|p| p.get("sessionId"))
                            .and_then(serde_json::Value::as_str)
                    {
                        let req_id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        let turn_cancel = CancellationToken::new();
                        if let Err(err) = turn_hub
                            .start_turn(sid, principal_id.clone(), turn_cancel.clone())
                            .await
                        {
                            let resp = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "error": {
                                    "code": SESSION_BUSY,
                                    "message": err.to_string(),
                                }
                            });
                            let _ = wire_tx.send(resp.to_string()).await;
                            continue;
                        }
                        active_prompts
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(req_id.to_string(), sid.to_string());
                        let server_for_cancel = Arc::clone(&server);
                        zeroclaw_spawn::spawn!(async move {
                            turn_cancel.cancelled().await;
                            server_for_cancel.cancel_active_turns();
                        });
                    }
                }

                if input_tx.send(text).await.is_err() {
                    break;
                }
            }
        }
    }

    drop(input_tx);

    // If the socket closed cleanly without revocation while a turn is still
    // running, let `server_task` and `output_bridge` finish in the background
    // so the turn completes, persists its transcript, and streams to any
    // reattached socket! On revocation, wait for the cancelled turn to unwind.
    if revoked {
        if let Err(e) = server_task.await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "ACP WebSocket server task panicked"
            );
        }
        let _ = output_bridge.await;
    } else {
        zeroclaw_spawn::spawn!(async move {
            let _ = server_task.await;
            let _ = output_bridge.await;
        });
    }
    let _ = output_task.await;
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "ACP WebSocket disconnected"
    );
}

fn extract_ws_token<'a>(headers: &'a HeaderMap, query_token: Option<&'a str>) -> Option<&'a str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .or_else(|| {
            headers
                .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|v| v.to_str().ok())
                .and_then(|protocols| {
                    protocols
                        .split(',')
                        .map(str::trim)
                        .find_map(|p| p.strip_prefix("bearer."))
                })
                .filter(|token| !token.is_empty())
        })
        .or_else(|| query_token.filter(|token| !token.is_empty()))
}

#[cfg(test)]
mod tests {
    //! Front-door proof for the gateway ACP surface.
    //!
    //! The `session/new` unit tests in `zeroclaw-channels` call
    //! `handle_session_new` directly; they prove the shared handler but cross
    //! neither user boundary. This test drives the *real* `/acp` WebSocket
    //! route: it boots the full gateway via `run_gateway` (so the request
    //! passes through the axum router, the pairing/subprotocol middleware in
    //! `handle_ws_acp`, the `on_upgrade` bridge, and `run_messages`), then
    //! connects a real WebSocket client and issues `session/new` with an
    //! omitted `cwd`. It asserts the returned `workspaceDir` is exactly the
    //! per-agent workspace — the behavior this PR introduces — rather than the
    //! daemon process CWD.

    use futures_util::{SinkExt, StreamExt};
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::Message;

    /// On-disk-equivalent of the channels-crate `make_test_config`: a fake
    /// `anthropic.default` provider (model name only, no key — agent
    /// construction is offline) and one dispatchable agent `test-agent`, with
    /// `config_path` pinned so `agent_workspace_dir` resolves under the temp
    /// install root. Pairing is disabled so the WS client needs no token.
    fn front_door_config(install_root: &std::path::Path) -> zeroclaw_config::schema::Config {
        use zeroclaw_config::schema::{
            AliasedAgentConfig, AnthropicModelProviderConfig, Config, ModelProviderConfig,
            RiskProfileConfig, RuntimeProfileConfig,
        };

        let mut cfg = Config {
            data_dir: install_root.join("data"),
            config_path: install_root.join("config.toml"),
            ..Config::default()
        };
        cfg.gateway.require_pairing = false;
        cfg.providers.models.anthropic.insert(
            "default".to_string(),
            AnthropicModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("claude-haiku-4-5".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        cfg.risk_profiles
            .insert("default".to_string(), RiskProfileConfig::default());
        cfg.runtime_profiles
            .insert("default".to_string(), RuntimeProfileConfig::default());
        cfg.agents.insert(
            "test-agent".to_string(),
            AliasedAgentConfig {
                model_provider: "anthropic.default".into(),
                risk_profile: "default".into(),
                runtime_profile: "default".into(),
                ..Default::default()
            },
        );
        cfg
    }

    #[tokio::test]
    async fn acp_ws_front_door_omitted_cwd_uses_agent_workspace() {
        check_acp_front_door(false).await;
    }

    #[tokio::test]
    async fn acp_ws_cutover_restores_legacy_dashboard_session() {
        check_acp_front_door(true).await;
    }

    async fn check_acp_front_door(cutover: bool) {
        // `run_gateway` binds the process-global pricing config handle.
        let _pricing_binding = crate::PRICING_BINDING_TEST_LOCK.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let install_root = tmp.path();
        let mut cfg = front_door_config(install_root);
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        cfg.gateway.acp_webchat_cutover = cutover;
        if cutover {
            use zeroclaw_api::model_provider::ChatMessage;
            cfg.channels.session_backend = "sqlite".into();
            let legacy = zeroclaw_infra::make_session_backend(&cfg.data_dir, "sqlite").unwrap();
            legacy
                .append("gw_team_alpha", &ChatMessage::user("legacy question"))
                .unwrap();
            legacy
                .append("gw_team_alpha", &ChatMessage::assistant("legacy answer"))
                .unwrap();
            legacy
                .set_session_agent_alias("gw_team_alpha", "test-agent")
                .unwrap();
        }
        let expected_ws = cfg
            .agent_workspace_dir("test-agent")
            .to_string_lossy()
            .into_owned();

        // Reserve an ephemeral port, then hand it to the gateway.
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, _reload_rx) = tokio::sync::watch::channel(false);
        let reload_controls = zeroclaw_runtime::daemon::GatewayReloadControls::standalone(
            shutdown_tx.clone(),
            reload_tx,
        );

        let server = zeroclaw_spawn::spawn!(crate::run_gateway(
            "127.0.0.1",
            port,
            cfg,
            None,
            Some(reload_controls),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ));

        // Wait until the gateway is accepting TCP connections.
        let addr = format!("127.0.0.1:{port}");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("gateway should accept connections");

        let url = format!("ws://127.0.0.1:{port}/acp");
        if cutover {
            let legacy = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/chat")).await;
            assert!(
                matches!(
                    legacy,
                    Err(tokio_tungstenite::tungstenite::Error::Http(ref resp))
                        if resp.status() == axum::http::StatusCode::GONE
                ),
                "legacy /ws/chat must return 410 Gone when acp_webchat_cutover is enabled"
            );
        }
        let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("WebSocket upgrade on /acp should succeed");

        ws.send(Message::Text(
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let req = if cutover {
            serde_json::json!({
                "jsonrpc":"2.0","id":2,"method":"session/load",
                "params":{"sessionId":"team_alpha"}
            })
        } else {
            serde_json::json!({
                "jsonrpc":"2.0","id":2,"method":"session/new",
                "params":{"agentAlias":"test-agent"}
            })
        };
        ws.send(Message::Text(req.to_string().into()))
            .await
            .unwrap();

        let mut replay = Vec::new();
        let workspace_dir = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(msg) = ws.next().await {
                let text = match msg {
                    Ok(Message::Text(t)) => t.to_string(),
                    Ok(_) => continue,
                    Err(e) => panic!("WebSocket read error: {e}"),
                };
                let value: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if value.get("id").and_then(|i| i.as_i64()) == Some(2) {
                    if let Some(err) = value.get("error") {
                        panic!("session request returned an error: {err}");
                    }
                    assert!(value.get("result").is_some());
                    return value["result"]["workspaceDir"].as_str().map(String::from);
                }
                if value["method"] == "session/update"
                    && value["params"]["sessionId"] == "team_alpha"
                    && let Some(text) = value["params"]["update"]["content"]["text"].as_str()
                {
                    replay.push(text.to_owned());
                }
            }
            panic!("ACP socket closed before the session response");
        })
        .await
        .expect("session response should arrive before timeout");

        shutdown_tx.send(true).ok();
        server.abort();

        if cutover {
            assert_eq!(replay, ["legacy question", "legacy answer"]);
            return;
        }
        assert_eq!(
            workspace_dir.as_deref(),
            Some(expected_ws.as_str()),
            "omitted-cwd session/new over the real /acp WebSocket route must \
             return the per-agent workspace, not the daemon CWD"
        );
    }

    #[tokio::test]
    async fn acp_turn_hub_replays_missed_tool_calls_and_final_response_without_duplicates() {
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let hub = super::AcpTurnHub::for_queue(&queue);
        let cancel = tokio_util::sync::CancellationToken::new();

        hub.start_turn("sess-reload", Some("alice".into()), cancel)
            .await
            .expect("first turn must acquire permit");

        // Emit 3 tool calls + partial output before the client reloads.
        assert_eq!(
            hub.record_frame("sess-reload", "tool_call:1".into(), false),
            Some(1)
        );
        assert_eq!(
            hub.record_frame("sess-reload", "tool_result:1".into(), false),
            Some(2)
        );
        assert_eq!(
            hub.record_frame("sess-reload", "tool_call:2".into(), false),
            Some(3)
        );

        // Client reloads and reattaches after seq=1 (saw tool_call:1 before reload).
        let mut attached = hub
            .reattach("sess-reload", Some("alice"), 1)
            .expect("owner reattach must succeed")
            .expect("turn is still active");
        assert!(!attached.completed);
        let mut observed: Vec<String> = attached.replay.iter().map(|e| e.payload.clone()).collect();
        let mut last_seq = attached.replay.last().map_or(1, |e| e.seq);

        // Turn continues live after reload with another tool call and terminal response.
        assert_eq!(
            hub.record_frame("sess-reload", "tool_result:2".into(), false),
            Some(4)
        );
        assert_eq!(
            hub.record_frame("sess-reload", "final_response".into(), true),
            Some(5)
        );
        hub.finish_turn("sess-reload");

        while let Ok(ev) = attached.live.recv().await {
            if ev.seq <= last_seq {
                continue;
            }
            last_seq = ev.seq;
            observed.push(ev.payload);
            if ev.is_terminal {
                break;
            }
        }

        assert_eq!(
            observed,
            [
                "tool_result:1",
                "tool_call:2",
                "tool_result:2",
                "final_response"
            ],
            "reattach must replay missed frames in exact order with zero duplicates"
        );
        assert!(
            hub.reattach("sess-reload", Some("alice"), 0)
                .unwrap()
                .is_none(),
            "finished turn must be removed from the active hub"
        );
    }

    #[tokio::test]
    async fn acp_turn_hub_enforces_principal_isolation_and_allows_owner_cancel() {
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let hub = super::AcpTurnHub::for_queue(&queue);
        let cancel = tokio_util::sync::CancellationToken::new();

        hub.start_turn("sess-scoped", Some("alice".into()), cancel.clone())
            .await
            .unwrap();

        assert_eq!(
            hub.reattach("sess-scoped", Some("bob"), 0).err(),
            Some(zeroclaw_api::jsonrpc::error_codes::FORBIDDEN)
        );
        assert_eq!(
            hub.reattach("sess-scoped", None, 0).err(),
            Some(zeroclaw_api::jsonrpc::error_codes::FORBIDDEN)
        );
        assert_eq!(
            hub.cancel_turn("sess-scoped", Some("bob")).err(),
            Some(zeroclaw_api::jsonrpc::error_codes::FORBIDDEN)
        );
        assert!(!cancel.is_cancelled());

        assert_eq!(hub.cancel_turn("sess-scoped", Some("alice")), Ok(true));
        assert!(cancel.is_cancelled());
        hub.finish_turn("sess-scoped");
    }

    #[tokio::test]
    async fn acp_turn_hub_uses_session_actor_queue_serialization() {
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            1, 1, 60,
        ));
        let hub = super::AcpTurnHub::for_queue(&queue);

        hub.start_turn(
            "sess-serial",
            None,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();

        let err = hub
            .start_turn(
                "sess-serial",
                None,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect_err("second concurrent turn on same session must be rejected by queue limit");
        assert!(matches!(
            err,
            zeroclaw_infra::session_queue::SessionQueueError::QueueFull { .. }
        ));

        hub.finish_turn("sess-serial");

        hub.start_turn(
            "sess-serial",
            None,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("after finish_turn releases permit, next turn must succeed");
        hub.finish_turn("sess-serial");
    }

    #[tokio::test]
    async fn acp_ws_rejects_unauthenticated_when_pairing_required() {
        let _pricing_binding = crate::PRICING_BINDING_TEST_LOCK.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = front_door_config(tmp.path());
        cfg.gateway.require_pairing = true;
        std::fs::create_dir_all(&cfg.data_dir).unwrap();

        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, _reload_rx) = tokio::sync::watch::channel(false);
        let reload_controls = zeroclaw_runtime::daemon::GatewayReloadControls::standalone(
            shutdown_tx.clone(),
            reload_tx,
        );

        let server = zeroclaw_spawn::spawn!(crate::run_gateway(
            "127.0.0.1",
            port,
            cfg,
            None,
            Some(reload_controls),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ));

        let addr = format!("127.0.0.1:{port}");
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("gateway should accept connections");

        let resp = tokio_tungstenite::connect_async(format!("ws://{addr}/acp")).await;
        assert!(
            matches!(
                resp,
                Err(tokio_tungstenite::tungstenite::Error::Http(ref http))
                    if http.status() == axum::http::StatusCode::UNAUTHORIZED
            ),
            "unauthenticated /acp upgrade must return 401 when pairing is required"
        );

        shutdown_tx.send(true).ok();
        server.abort();
    }

    #[tokio::test]
    async fn acp_ws_graceful_shutdown_closes_idle_socket() {
        let _pricing_binding = crate::PRICING_BINDING_TEST_LOCK.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let cfg = front_door_config(tmp.path());
        std::fs::create_dir_all(&cfg.data_dir).unwrap();

        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        let (reload_tx, _reload_rx) = tokio::sync::watch::channel(false);
        let reload_controls = zeroclaw_runtime::daemon::GatewayReloadControls::standalone(
            shutdown_tx.clone(),
            reload_tx,
        );

        let server = zeroclaw_spawn::spawn!(crate::run_gateway(
            "127.0.0.1",
            port,
            cfg,
            None,
            Some(reload_controls),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ));

        let addr = format!("127.0.0.1:{port}");
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("gateway should accept connections");

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/acp"))
            .await
            .expect("WS upgrade should succeed");

        shutdown_tx.send(true).ok();

        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(frame) = ws.next().await {
                if matches!(frame, Ok(Message::Close(_)) | Err(_)) {
                    return true;
                }
            }
            true
        })
        .await
        .expect("idle /acp socket should close promptly on graceful shutdown");
        assert!(closed);
        server.abort();
    }
}
