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
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex, Weak};
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
    events: VecDeque<AcpTurnEventFrame>,
    identity: uuid::Uuid,
    server: Weak<AcpServer>,
    pending: HashMap<String, AcpTurnEventFrame>,
    next_seq: u64,
    completed: bool,
    tx: broadcast::Sender<AcpTurnEventFrame>,
    cancel: CancellationToken,
    guard: Option<SessionGuard>,
}

/// Snapshot + live receiver returned when a reconnected socket reattaches to
/// an in-flight ACP turn.
pub(crate) struct AcpTurnAttachment {
    origin: Weak<AcpServer>,
    pub(crate) replay: Vec<AcpTurnEventFrame>,
    pub(crate) live: broadcast::Receiver<AcpTurnEventFrame>,
    pub(crate) completed: bool,
    pub(crate) turn_id: uuid::Uuid,
    pub(crate) truncated: bool,
}

impl AcpTurnAttachment {
    /// Replace this socket's previous reader before acknowledging the new cursor.
    async fn send_to_socket(
        self,
        sid: &str,
        after_seq: u64,
        request_id: serde_json::Value,
        reader: &mut Option<tokio::task::JoinHandle<()>>,
        wire: &mpsc::Sender<String>,
        socket_server: &Arc<AcpServer>,
    ) {
        if let Some(old) = reader.take() {
            old.abort();
            let _ = old.await;
        }
        let ack = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "sessionId": sid,
                "inFlight": !self.completed,
                "replayed": self.replay.len(),
                "truncated": self.truncated,
                "turnId": self.turn_id.to_string(),
            }
        });
        if wire.send(ack.to_string()).await.is_err() {
            return;
        }
        // The originating socket already receives live output, including the
        // JSON-RPC response with its original prompt id, from its output bridge.
        let needs_live = !self.completed && !self.origin.ptr_eq(&Arc::downgrade(socket_server));
        if self.replay.is_empty() && !needs_live {
            return;
        }
        let sid = sid.to_owned();
        let wire = wire.clone();
        *reader = Some(zeroclaw_spawn::spawn!(async move {
            let mut last_seq = after_seq;
            for event in self.replay {
                last_seq = last_seq.max(event.seq);
                let done = event.is_terminal;
                if wire.send(replay_payload(&sid, event)).await.is_err() || done {
                    return;
                }
            }
            if !needs_live {
                return;
            }
            let mut rx = self.live;
            loop {
                match rx.recv().await {
                    Ok(event) if event.seq > last_seq => {
                        last_seq = event.seq;
                        let done = event.is_terminal;
                        if wire.send(replay_payload(&sid, event)).await.is_err() || done {
                            break;
                        }
                    }
                    Ok(_) => (),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let gap = serde_json::json!({"jsonrpc":"2.0","method":"_zeroclaw/replay_gap","params":{"sessionId":sid,"afterSeq":last_seq}});
                        let _ = wire.send(gap.to_string()).await;
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
    }
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

    /// Acquire the per-session [`SessionActorQueue`] permit and register an
    /// active turn for `session_id`.
    pub(crate) async fn start_turn(
        &self,
        session_id: &str,
        principal_id: Option<String>,
        cancel: CancellationToken,
        server: Weak<AcpServer>,
    ) -> Result<(), SessionQueueError> {
        let permit = self.queue.acquire(session_id).await?;
        let (tx, _) = broadcast::channel(256);
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        turns.insert(
            session_id.to_string(),
            ActiveAcpTurn {
                principal_id,
                events: VecDeque::new(),
                identity: uuid::Uuid::new_v4(),
                server,
                pending: HashMap::new(),
                next_seq: 1,
                completed: false,
                tx,
                cancel,
                guard: Some(permit),
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
    ) -> Option<AcpTurnEventFrame> {
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let turn = turns.get_mut(session_id)?;
        let seq = turn.next_seq;
        turn.next_seq = turn.next_seq.saturating_add(1);
        let mut parsed: serde_json::Value = serde_json::from_str(&payload).ok()?;
        parsed["_meta"]["zeroclawTurn"] = serde_json::json!({"sessionId":session_id,"turnId":turn.identity.to_string(),"seq":seq});
        let request_id = parsed
            .get("method")
            .and_then(|_| parsed.get("id"))
            .and_then(|id| id.as_str())
            .map(str::to_owned);
        if let Some(id) = &request_id {
            parsed["id"] = format!("{}:{id}", turn.identity).into();
        }
        let payload = parsed.to_string();
        let frame = AcpTurnEventFrame {
            seq,
            payload,
            is_terminal,
        };
        if let Some(id) = request_id {
            turn.pending.insert(id, frame.clone());
        }
        if turn.events.len() == MAX_BUFFERED_TURN_EVENTS {
            turn.events.pop_front();
        }
        turn.events.push_back(frame.clone());
        if is_terminal {
            turn.completed = true;
        }
        let _ = turn.tx.send(frame.clone());
        Some(frame)
    }

    /// Release admission immediately but keep the terminal response available across a reload.
    pub(crate) fn finish_turn(self: &Arc<Self>, session_id: &str) {
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(turn) = turns.get_mut(session_id) else {
            return;
        };
        turn.guard.take();
        turn.pending.clear();
        turn.completed = true;
        turn.server = Weak::new();
        let identity = turn.identity;
        let hub = Arc::downgrade(self);
        let sid = session_id.to_owned();
        zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            if let Some(hub) = hub.upgrade() {
                let mut turns = hub.turns.lock().unwrap_or_else(|e| e.into_inner());
                if turns
                    .get(&sid)
                    .is_some_and(|turn| turn.identity == identity)
                {
                    turns.remove(&sid);
                }
            }
        });
    }

    fn answer(
        &self,
        session_id: &str,
        principal: Option<&str>,
        id: &str,
        response: &serde_json::Value,
    ) -> bool {
        let Some((identity, request)) = id.split_once(':') else {
            return false;
        };
        let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(turn) = turns.get_mut(session_id) else {
            return false;
        };
        if turn.principal_id.as_deref() != principal
            || turn.identity.to_string() != identity
            || turn.pending.remove(request).is_none()
        {
            return false;
        }
        if let Some(server) = turn.server.upgrade() {
            server.answer_request(request, response);
            return true;
        }
        false
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
        active_only: bool,
        accept_partial: bool,
    ) -> Result<Option<AcpTurnAttachment>, i32> {
        let turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(turn) = turns.get(session_id) else {
            return Ok(None);
        };
        if turn.principal_id.as_deref() != principal_id {
            return Err(FORBIDDEN);
        }
        if active_only && turn.completed {
            return Ok(None);
        }
        let truncated = turn
            .events
            .front()
            .is_some_and(|e| e.seq > after_seq.saturating_add(1));
        if truncated && !accept_partial {
            return Err(zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS);
        }
        let mut replay: Vec<_> = turn
            .events
            .iter()
            .filter(|e| {
                e.seq > after_seq && {
                    let request = serde_json::from_str::<serde_json::Value>(&e.payload)
                        .ok()
                        .is_some_and(|v| v.get("method").is_some() && v.get("id").is_some());
                    !request || turn.pending.values().any(|pending| pending.seq == e.seq)
                }
            })
            .cloned()
            .collect();
        // Include unanswered requests even when they have left the ring.
        let first_buffered = turn.events.front().map_or(turn.next_seq, |frame| frame.seq);
        replay.extend(
            turn.pending
                .values()
                .filter(|e| e.seq <= after_seq || e.seq < first_buffered)
                .cloned(),
        );
        replay.sort_by_key(|e| e.seq);
        Ok(Some(AcpTurnAttachment {
            origin: turn.server.clone(),
            replay,
            live: turn.tx.subscribe(),
            completed: turn.completed,
            turn_id: turn.identity,
            truncated,
        }))
    }

    fn running_server(&self, session_id: &str, owner: Option<&str>) -> Option<Arc<AcpServer>> {
        let turns = self.turns.lock().unwrap_or_else(|error| error.into_inner());
        let turn = turns.get(session_id)?;
        if turn.completed || turn.principal_id.as_deref() != owner {
            return None;
        }
        turn.server.upgrade()
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

pub(crate) async fn handle_ws_acp(
    Extension(turn_hub): Extension<Arc<AcpTurnHub>>,
    State(state): State<AppState>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    Query(params): Query<AcpQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let mut session_watch = None;
    let mut principal_id = None;
    let mut scoped_grants: Option<
        Arc<dyn Fn() -> Option<zeroclaw_api::grants::ResolvedGrants> + Send + Sync>,
    > = None;

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
                        let Some(agent) = params.agent.as_deref() else {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                "Scoped chat requires an agent",
                            )
                                .into_response();
                        };
                        if !crate::dashboard_core::may_chat(&session.grants)
                            || !session.grants.may_use_agent(agent)
                        {
                            return (axum::http::StatusCode::FORBIDDEN, "Chat access denied")
                                .into_response();
                        }
                        let auth_for_grants = Arc::clone(&auth);
                        let session_for_grants = Arc::clone(&session);
                        scoped_grants = Some(Arc::new(move || {
                            auth_for_grants.chat_grants(&session_for_grants)
                        }));
                        session_watch =
                            Some(SessionWatch::for_acp_chat(auth, session, agent, &state));
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
        handle_socket(
            socket,
            state,
            params.agent,
            session_watch,
            principal_id,
            scoped_grants,
            turn_hub,
        )
    })
    .into_response()
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    default_agent: Option<String>,
    mut session_watch: Option<SessionWatch>,
    principal_id: Option<String>,
    scoped_grants: Option<
        Arc<dyn Fn() -> Option<zeroclaw_api::grants::ResolvedGrants> + Send + Sync>,
    >,
    turn_hub: Arc<AcpTurnHub>,
) {
    let (mut sender, mut receiver) = socket.split();
    let (input_tx, input_rx) = mpsc::channel::<String>(256);
    let (output_tx, mut output_rx) = mpsc::channel::<String>(256);
    let (wire_tx, mut wire_rx) = mpsc::channel::<String>(256);

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
            .with_scoped_grants(scoped_grants)
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
            .with_scoped_grants(scoped_grants)
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

    let local_generations = Arc::new(StdMutex::new(HashMap::<String, u64>::new()));
    let output_generations = Arc::clone(&local_generations);
    let hub_for_output = Arc::clone(&turn_hub);
    let prompts_for_output = Arc::clone(&active_prompts);
    let wire_tx_for_output = wire_tx.clone();
    let output_bridge = zeroclaw_spawn::spawn!(async move {
        while let Some(mut line) = output_rx.recv().await {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(method) = parsed.get("method").and_then(serde_json::Value::as_str)
                    && (method == "session/update"
                        || method.starts_with("_zeroclaw/")
                        || method == "session/request_permission"
                        || method == "elicitation/create")
                    && let Some(sid) = parsed
                        .get("params")
                        .and_then(|p| p.get("sessionId").or_else(|| p.get("session_id")))
                        .and_then(serde_json::Value::as_str)
                {
                    if prompts_for_output
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .values()
                        .any(|v| v == sid)
                        && let Some(frame) = hub_for_output.record_frame(sid, line.clone(), false)
                    {
                        line = frame.payload;
                    }
                } else if let Some(id) = parsed.get("id")
                    && (parsed.get("result").is_some() || parsed.get("error").is_some())
                {
                    let id_key = id.to_string();
                    let finished_sid = prompts_for_output
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id_key);
                    if let Some(sid) = finished_sid {
                        if let Some(frame) = hub_for_output.record_frame(&sid, line.clone(), true) {
                            line = frame.payload;
                        }
                        let generation = hub_for_output.queue.advance_generation(&sid);
                        output_generations
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(sid.clone(), generation);
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

    let revoked_token = CancellationToken::new();
    let disconnected = CancellationToken::new();
    // A socket has at most one replay reader per admitted session.
    let mut attached_sessions = HashMap::<String, Option<tokio::task::JoinHandle<()>>>::new();

    let mut revoked = false;
    loop {
        tokio::select! {
            code = session_lost(&mut session_watch, &state) => {
                revoked = true;
                revoked_token.cancel();
                server.cancel_active_turns();
                *close_code.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((code, if code == WS_CLOSE_SESSION_ENDED { "dashboard session ended" } else { "access revoked" }));
                close_signal.notify_one();
                break;
            }
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    revoked_token.cancel();
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

                if let Err(code) = crate::dashboard_session::session_admits_now(&mut session_watch, &state).await {
                    revoked = true;
                    revoked_token.cancel();
                    server.cancel_active_turns();
                    *close_code.lock().unwrap_or_else(|e|e.into_inner()) = Some((code,"access revoked"));
                    break;
                }
                if let Ok(reply) = serde_json::from_str::<serde_json::Value>(&text)
                    && (reply.get("result").is_some() || reply.get("error").is_some())
                    && let Some(id) = reply["id"].as_str()
                    && attached_sessions.keys().any(|sid| turn_hub.answer(sid,principal_id.as_deref(),id,&reply)) {
                    continue;
                }
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
                            .and_then(|p| p.get("sessionId").or_else(||p.get("session_id")))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        let after_seq = parsed
                            .get("params")
                            .and_then(|p| p.get("afterSeq"))
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        if !server.has_gateway_session(sid).await {
                            let _ = wire_tx.send(serde_json::json!({"jsonrpc":"2.0","id":req_id,"error":{"code":FORBIDDEN,"message":"Restore the session before reattaching"}}).to_string()).await;
                            continue;
                        }
                        let reader = attached_sessions.entry(sid.to_owned()).or_default();
                        let params = &parsed["params"];
                        let active_only = params["activeOnly"].as_bool().unwrap_or(false);
                        let accept_partial = params["acceptPartial"].as_bool().unwrap_or(false);
                        match turn_hub.reattach(sid, principal_id.as_deref(), after_seq, active_only, accept_partial) {
                            Ok(Some(attachment)) => {
                                attachment.send_to_socket(sid, after_seq, req_id, reader, &wire_tx, &server).await;
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
                    if method == "_zeroclaw/session/steer"
                        && parsed["jsonrpc"] == "2.0"
                        && let Some(sid) = parsed["params"].get("sessionId").or_else(|| parsed["params"].get("session_id")).and_then(serde_json::Value::as_str)
                        && attached_sessions.contains_key(sid)
                        && server.has_gateway_session(sid).await
                        && let Some(owner) = turn_hub.running_server(sid, principal_id.as_deref())
                    {
                        let response = owner.gateway_steer(&parsed["params"], parsed["id"].clone()).await;
                        if parsed.get("id").is_some() {
                            let _ = wire_tx.send(serde_json::json!(response).to_string()).await;
                        }
                        continue;
                    }
                    if method == "session/cancel"
                        && let Some(sid) = parsed
                            .get("params")
                            .and_then(|p| p.get("sessionId").or_else(||p.get("session_id")))
                            .and_then(serde_json::Value::as_str)
                    {
                        if server.has_gateway_session(sid).await {
                            let _ = turn_hub.cancel_turn(sid, principal_id.as_deref());
                        }
                    }
                    if method == "session/prompt"
                        && let Some(sid) = parsed
                            .get("params")
                            .and_then(|p| p.get("sessionId").or_else(||p.get("session_id")))
                            .and_then(serde_json::Value::as_str)
                    {
                        let req_id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        if req_id.is_null() || parsed["jsonrpc"] != "2.0" || !server.has_gateway_session(sid).await {
                            let _ = wire_tx.send(serde_json::json!({"jsonrpc":"2.0","id":req_id,"error":{"code":zeroclaw_api::jsonrpc::error_codes::INVALID_REQUEST,"message":"Prompt requires an id and an open session"}}).to_string()).await;
                            continue;
                        }
                        let sid = sid.to_owned();
                        attached_sessions.entry(sid.clone()).or_default();
                        let params = parsed["params"].clone();
                        let turn_cancel = revoked_token.child_token();
                        let hub = Arc::clone(&turn_hub);
                        let server = Arc::clone(&server);
                        let wire = wire_tx.clone();
                        let principal = principal_id.clone();
                        let prompts = Arc::clone(&active_prompts);
                        let closed = disconnected.clone();
                        let generations = Arc::clone(&local_generations);
                        zeroclaw_spawn::spawn!(async move {
                            let admitted = tokio::select! {
                                biased;
                                () = closed.cancelled() => return,
                                result = hub.start_turn(&sid,principal,turn_cancel.clone(),Arc::downgrade(&server)) => result,
                            };
                            if let Err(e) = admitted {
                                let _ = wire.send(serde_json::json!({"jsonrpc":"2.0","id":req_id,"error":{"code":SESSION_BUSY,"message":e.to_string()}}).to_string()).await;
                                return;
                            }
                            let generation = hub.queue.generation(&sid);
                            let stale = generations.lock().unwrap_or_else(|e|e.into_inner()).get(&sid).copied().unwrap_or(0) != generation;
                            prompts.lock().unwrap_or_else(|e|e.into_inner()).insert(req_id.to_string(),sid.clone());
                            server.gateway_prompt(req_id,&params,turn_cancel,stale).await;

                        });
                        continue;
                    }
                }

                if input_tx.send(text).await.is_err() {
                    break;
                }
            }
        }
    }

    for reader in attached_sessions.into_values().flatten() {
        reader.abort();
    }
    disconnected.cancel();
    close_signal.notify_one();
    drop(input_tx);
    drop(server);
    drop(wire_tx);
    zeroclaw_spawn::spawn!(async move {
        let _ = server_task.await;
        let mut bridge = output_bridge;
        if revoked {
            let _ = bridge.await;
        } else {
            tokio::select! {
                _ = &mut bridge => (),
                _ = session_lost(&mut session_watch,&state) => {
                    revoked_token.cancel();
                    let _ = bridge.await;
                }
                _ = shutdown_rx.changed() => {
                    revoked_token.cancel();
                    let _ = bridge.await;
                }
            }
        }
    });
    let _ = output_task.await;
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "ACP WebSocket disconnected"
    );
}

fn replay_payload(sid: &str, event: AcpTurnEventFrame) -> String {
    if !event.is_terminal {
        return event.payload;
    }
    let response: serde_json::Value =
        serde_json::from_str(&event.payload).expect("hub stores encoded JSON");
    serde_json::json!({"jsonrpc":"2.0","method":"_zeroclaw/turn_complete","params":{"sessionId":sid,"result":response["result"],"error":response["error"]},"_meta":response["_meta"]}).to_string()
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
        // `run_gateway` binds the process-global pricing config handle.
        let _pricing_binding = crate::PRICING_BINDING_TEST_LOCK.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let install_root = tmp.path();
        let cfg = front_door_config(install_root);
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
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
        let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("WebSocket upgrade on /acp should succeed");

        ws.send(Message::Text(
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        ws.send(Message::Text(
            serde_json::json!({
                "jsonrpc":"2.0","id":2,"method":"session/new",
                "params":{"agentAlias":"test-agent"}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

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
                        panic!("session/new returned an error: {err}");
                    }
                    return value["result"]["workspaceDir"].as_str().map(String::from);
                }
            }
            None
        })
        .await
        .expect("session/new response should arrive before timeout");

        shutdown_tx.send(true).ok();
        server.abort();

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
        let hub = Arc::new(super::AcpTurnHub::new(queue.clone()));
        let cancel = tokio_util::sync::CancellationToken::new();

        hub.start_turn(
            "sess-reload",
            Some("alice".into()),
            cancel,
            std::sync::Weak::new(),
        )
        .await
        .expect("first turn must acquire permit");

        // Emit 3 tool calls + partial output before the client reloads.
        assert_eq!(
            hub.record_frame(
                "sess-reload",
                serde_json::json!({"data":"tool_call:1"}).to_string(),
                false
            )
            .map(|f| f.seq),
            Some(1)
        );
        assert_eq!(
            hub.record_frame(
                "sess-reload",
                serde_json::json!({"data":"tool_result:1"}).to_string(),
                false
            )
            .map(|f| f.seq),
            Some(2)
        );
        assert_eq!(
            hub.record_frame(
                "sess-reload",
                serde_json::json!({"data":"tool_call:2"}).to_string(),
                false
            )
            .map(|f| f.seq),
            Some(3)
        );

        // Client reloads and reattaches after seq=1 (saw tool_call:1 before reload).
        let mut attached = hub
            .reattach("sess-reload", Some("alice"), 1, false, false)
            .expect("owner reattach must succeed")
            .expect("turn is still active");
        assert!(!attached.completed);
        let mut observed: Vec<String> = attached
            .replay
            .iter()
            .map(|e| {
                serde_json::from_str::<serde_json::Value>(&e.payload).unwrap()["data"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let mut last_seq = attached.replay.last().map_or(1, |e| e.seq);

        // Turn continues live after reload with another tool call and terminal response.
        assert_eq!(
            hub.record_frame(
                "sess-reload",
                serde_json::json!({"data":"tool_result:2"}).to_string(),
                false
            )
            .map(|f| f.seq),
            Some(4)
        );
        assert_eq!(
            hub.record_frame(
                "sess-reload",
                serde_json::json!({"data":"final_response"}).to_string(),
                true
            )
            .map(|f| f.seq),
            Some(5)
        );
        hub.finish_turn("sess-reload");

        while let Ok(ev) = attached.live.recv().await {
            if ev.seq <= last_seq {
                continue;
            }
            last_seq = ev.seq;
            observed.push(
                serde_json::from_str::<serde_json::Value>(&ev.payload).unwrap()["data"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
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
            hub.reattach("sess-reload", Some("alice"), 0, false, false)
                .unwrap()
                .is_some_and(|a| a.completed),
            "terminal replay must survive completion"
        );
    }

    #[tokio::test]
    async fn reattach_retry_delivers_live_frames_once_only_to_non_origin_reader() {
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let hub = super::AcpTurnHub::new(queue);
        let origin = Arc::new(super::AcpServer::new(
            Default::default(),
            Default::default(),
        ));
        let observer = Arc::new(super::AcpServer::new(
            Default::default(),
            Default::default(),
        ));
        let sid = "reattach-retry";
        hub.start_turn(sid, None, Default::default(), Arc::downgrade(&origin))
            .await
            .unwrap();
        hub.record_frame(sid, serde_json::json!({"data": "first"}).to_string(), false)
            .unwrap();

        let attach_twice = async |server: &Arc<super::AcpServer>| {
            let (tx, mut rx) = tokio::sync::mpsc::channel(16);
            let mut reader = None;
            for cursor in [0, 1] {
                let attachment = hub
                    .reattach(sid, None, cursor, false, false)
                    .unwrap()
                    .unwrap();
                attachment
                    .send_to_socket(sid, cursor, cursor.into(), &mut reader, &tx, server)
                    .await;
                let ack: serde_json::Value =
                    serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
                assert_eq!(ack["result"]["replayed"], 1 - cursor);
                if cursor == 0 {
                    let replay: serde_json::Value =
                        serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
                    assert_eq!(replay["data"], "first");
                }
            }
            rx
        };
        let mut origin_wire = attach_twice(&origin).await;
        let mut observer_wire = attach_twice(&observer).await;

        hub.record_frame(
            sid,
            serde_json::json!({"data": "second"}).to_string(),
            false,
        )
        .unwrap();
        hub.record_frame(
            sid,
            serde_json::json!({"id": 42, "result": {}}).to_string(),
            true,
        )
        .unwrap();
        assert!(
            origin_wire.recv().await.is_none(),
            "origin already has the direct output bridge"
        );
        let update: serde_json::Value =
            serde_json::from_str(&observer_wire.recv().await.unwrap()).unwrap();
        assert_eq!(update["data"], "second");
        let terminal: serde_json::Value =
            serde_json::from_str(&observer_wire.recv().await.unwrap()).unwrap();
        assert_eq!(terminal["method"], "_zeroclaw/turn_complete");
        assert!(
            observer_wire.recv().await.is_none(),
            "old reader must not send duplicates"
        );
    }

    #[tokio::test]
    async fn acp_turn_hub_enforces_principal_isolation_and_allows_owner_cancel() {
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let hub = Arc::new(super::AcpTurnHub::new(queue.clone()));
        let cancel = tokio_util::sync::CancellationToken::new();

        hub.start_turn(
            "sess-scoped",
            Some("alice".into()),
            cancel.clone(),
            std::sync::Weak::new(),
        )
        .await
        .unwrap();

        assert_eq!(
            hub.reattach("sess-scoped", Some("bob"), 0, false, false)
                .err(),
            Some(zeroclaw_api::jsonrpc::error_codes::FORBIDDEN)
        );
        assert_eq!(
            hub.reattach("sess-scoped", None, 0, false, false).err(),
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
        let hub = Arc::new(super::AcpTurnHub::new(queue.clone()));

        hub.start_turn(
            "sess-serial",
            None,
            tokio_util::sync::CancellationToken::new(),
            std::sync::Weak::new(),
        )
        .await
        .unwrap();

        let err = hub
            .start_turn(
                "sess-serial",
                None,
                tokio_util::sync::CancellationToken::new(),
                std::sync::Weak::new(),
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
            std::sync::Weak::new(),
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

        let resp = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/acp")).await;
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

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/acp"))
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
