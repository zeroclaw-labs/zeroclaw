//! The dashboard chat socket, `GET /ws/chat`, for a signed-in dashboard
//! session, served through the core. The gateway translates the chat frame
//! protocol to the core's session methods and their `session/update` stream,
//! on the socket's own core connection. Each socket is one conversation,
//! created when it opens and closed when it closes ([`CoreChat`]). The core
//! runs every turn, keeps the conversation under the signed-in principal and
//! applies that principal's grants; the gateway holds nothing beyond the
//! socket.
//!
//! What the core does not offer this socket is refused with a frame that
//! says so, never approximated: steering a running turn, and SOP approvals,
//! whose decision the core would record under the wrong approver. A failed
//! turn is reported without its details, which stay in the core's log.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};
use zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED;
use zeroclaw_rpc_client::{ClientError, Method, Notification, RpcClient};
use zeroclaw_rpc_proto::notification::SESSION_UPDATE;
use zeroclaw_rpc_proto::types::{SessionUpdateEvent, TurnCompletionOutcome, TurnUsageTotals};

use crate::AppState;
use crate::dashboard_core::{
    CoreCaller, CoreChat, CoreFailure, agent_not_allowed, chat_not_permitted,
};
use crate::dashboard_session::{SessionWatch, WS_CLOSE_SESSION_ENDED, session_lost};
use crate::ws::{
    ConnectParams, WS_PROTOCOL, WsQuery, close_for_reconnect, close_with, gateway_stop_signal,
    history_trimmed_ws_frame, tick_websocket_ping, websocket_ping_interval,
};

/// The longest the gateway waits for a turn's answer. The core's own limits
/// end a turn long before this.
const TURN_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

/// Close reason for a socket whose dashboard session ended or lost access.
const SESSION_ENDED_REASON: &str = "dashboard session ended";

/// WebSocket close code after a `CONVERSATION_LIMIT` error frame: the
/// session already has as many chats open as it may, so no conversation was
/// created. A browser never learns why an upgrade was refused, so this is
/// said on an accepted socket.
pub(crate) const WS_CLOSE_CONVERSATION_LIMIT: u16 = 4409;

/// WebSocket close code after a `CONVERSATION_CLOSED` error frame: the
/// conversation was closed under the socket, by the core or an operator. A
/// new socket starts a new one.
pub(crate) const WS_CLOSE_CONVERSATION_CLOSED: u16 = 4410;

const CONVERSATION_CLOSED_MESSAGE: &str = "This conversation was closed; start a new one";

type Receiver = SplitStream<WebSocket>;

/// How long one frame may wait on a browser that stopped reading before the
/// socket counts as gone.
const WRITE_DEADLINE: Duration = Duration::from_secs(10);

/// The socket's sending half. Every write gives up when the session ends,
/// when the gateway generation stops, or when a browser that stopped reading
/// holds it past [`WRITE_DEADLINE`], so a chat stuck on a write still reaches
/// its close.
struct Sender {
    sink: SplitSink<WebSocket, Message>,
    ended: tokio_util::sync::CancellationToken,
    stopping: watch::Receiver<bool>,
}

impl Sender {
    fn new(
        sink: SplitSink<WebSocket, Message>,
        ended: tokio_util::sync::CancellationToken,
        state: &AppState,
    ) -> Self {
        Self {
            sink,
            ended,
            stopping: state.shutdown_tx.subscribe(),
        }
    }

    async fn write(&mut self, message: Message) -> Result<(), axum::Error> {
        let mut stopping = self.stopping.clone();
        let ended = &self.ended;
        let lost = async move {
            if *stopping.borrow() || stopping.has_changed().is_err() {
                return;
            }
            tokio::select! {
                () = ended.cancelled() => {}
                _ = stopping.changed() => {}
            }
        };
        tokio::select! {
            biased;
            () = lost => Err(axum::Error::new("the chat ended")),
            sent = tokio::time::timeout(WRITE_DEADLINE, self.sink.send(message)) => {
                sent.unwrap_or_else(|_| Err(axum::Error::new("the browser stopped reading")))
            }
        }
    }
}

/// Open the chat socket, with a conversation of its own on the core. The
/// agent is checked and the conversation created before the upgrade, so a
/// refusal is answered over HTTP, as on the in-process chat.
pub(crate) async fn ws_chat_through_core(
    caller: CoreCaller,
    state: AppState,
    params: WsQuery,
    headers: &HeaderMap,
    ws: WebSocketUpgrade,
    watch: Option<SessionWatch>,
) -> Response {
    let Some(agent_alias) = params.agent_alias.filter(|s| !s.trim().is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            "Missing required `agent` query parameter — pass `?agent=<alias>` matching a configured [agents.<alias>] entry.",
        )
            .into_response();
    };
    // Checked before the core sees the alias: an agent this account may not
    // use is refused without saying whether it exists.
    if !caller.session.grants.may_use_agent(&agent_alias) {
        return agent_not_allowed();
    }
    if !crate::dashboard_core::may_chat(&caller.session.grants) {
        return chat_not_permitted();
    }
    if state.config.read().agent(&agent_alias).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            format!("Unknown agent `{agent_alias}` — no [agents.{agent_alias}] entry configured."),
        )
            .into_response();
    }
    // Every socket starts a conversation of its own: a dashboard id in the
    // URL names nothing here.
    let cwd = params.cwd.or(params.workspace_dir);
    let requests_protocol = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == WS_PROTOCOL));
    let ws = if requests_protocol {
        ws.protocols([WS_PROTOCOL])
    } else {
        ws
    };
    let mut core = match caller.open_chat(&agent_alias, cwd.as_deref()).await {
        Ok(core) => core,
        // Said on the socket: the browser would see only a dropped one.
        Err(CoreFailure::ConversationLimit) => {
            let ended = caller.session.ended.clone();
            return ws
                .on_upgrade(move |socket| async move {
                    let (sink, mut receiver) = socket.split();
                    let mut sender = Sender::new(sink, ended, &state);
                    if let Some(stop) =
                        turn_failed(&mut sender, CoreFailure::ConversationLimit).await
                    {
                        finish(&mut sender, &mut receiver, stop).await;
                    }
                })
                .into_response();
        }
        Err(failure) => return failure.into_response(),
    };
    let name = params
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    if let Some(name) = &name {
        // A name is cosmetic: a refusal does not stop the chat.
        let session_id = core.core_id.clone();
        let _ = core
            .call::<Value>(
                Method::SessionRename,
                json!({ "session_id": session_id, "name": name }),
            )
            .await;
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Connect).with_attrs(
            ::serde_json::json!({
                "principal_id": caller.session.principal().id.as_str(),
                "admin": caller.session.is_admin(),
                "agent": agent_alias,
            })
        ),
        "dashboard chat opened through the core"
    );
    let chat = Chat { core, state, watch };
    // A socket that never upgrades drops the chat, which closes its
    // conversation.
    ws.on_upgrade(move |socket| chat.run(socket, name))
        .into_response()
}

/// How a socket ends.
enum Stop {
    /// The peer left, or the socket was already closed.
    Gone,
    /// Close with this code; the client must not reconnect on its own.
    Close(u16),
    /// The gateway generation is stopping: close so the client reconnects.
    Reconnect,
}

/// How a turn ended, for the socket.
enum TurnEnd {
    /// The socket stays open for the next message.
    Finished,
    Stop(Stop),
    /// The core asked for a new handshake before the turn ran.
    AuthRequired,
}

struct Chat {
    /// The socket's conversation, on its own connection.
    core: CoreChat,
    state: AppState,
    watch: Option<SessionWatch>,
}

impl Chat {
    /// Serve the socket, then close its conversation: it ends with the
    /// socket.
    async fn run(mut self, socket: WebSocket, name: Option<String>) {
        let (sink, mut receiver) = socket.split();
        let mut sender = Sender::new(sink, self.core.ended(), &self.state);
        let stop = self.serve(&mut sender, &mut receiver, name).await;
        let Chat { core, .. } = self;
        // The conversation closes while the browser is told, not after it
        // answers: a browser that never answers the close holds up neither.
        tokio::join!(core.close(), finish(&mut sender, &mut receiver, stop));
    }

    /// Serve the socket until it ends, and say how.
    async fn serve(
        &mut self,
        sender: &mut Sender,
        receiver: &mut Receiver,
        name: Option<String>,
    ) -> Stop {
        let mut start = json!({
            "type": "session_start",
            "session_id": self.core.core_id,
            "resumed": false,
            "message_count": 0,
        });
        if let Some(name) = name {
            start["name"] = Value::String(name);
        }
        if send(sender, &start).await.is_err() {
            return Stop::Gone;
        }
        let mut ping = websocket_ping_interval(&self.state.config.read());
        let mut stopping = self.state.shutdown_tx.subscribe();

        // The first frame may be a `connect` handshake; anything else is the
        // first message.
        let mut first_message = None;
        loop {
            let frame = tokio::select! {
                biased;
                frame = receiver.next() => frame,
                _ = gateway_stop_signal(&mut stopping) => return Stop::Reconnect,
                code = session_lost(&mut self.watch, &self.state) => return Stop::Close(code),
                _ = tick_websocket_ping(&mut ping) => {
                    if sender.write(Message::Ping(Vec::new().into())).await.is_err() {
                        return Stop::Gone;
                    }
                    continue;
                }
            };
            match frame {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ConnectParams>(&text) {
                        Ok(connect) if connect.msg_type == "connect" => {
                            // The conversation was bound to its working
                            // directory when it was opened, before this frame
                            // could arrive.
                            if connect.cwd.is_some() && connect.cwd.as_deref() != self.core.cwd() {
                                let error = json!({
                                    "type": "error",
                                    "message": "this chat fixes the session's working directory when the socket opens: pass `cwd` in the URL",
                                    "code": "INVALID_CWD",
                                });
                                let _ = send(sender, &error).await;
                                return Stop::Gone;
                            }
                            let ack = json!({
                                "type": "connected",
                                "message": "Connection established",
                            });
                            if send(sender, &ack).await.is_err() {
                                return Stop::Gone;
                            }
                        }
                        _ => first_message = Some(text.to_string()),
                    }
                    break;
                }
                Some(Ok(Message::Ping(payload))) => {
                    if sender.write(Message::Pong(payload)).await.is_err() {
                        return Stop::Gone;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return Stop::Gone,
                Some(Ok(_)) => {}
            }
        }

        if let Some(text) = first_message {
            let stop = self
                .on_frame(sender, receiver, &mut ping, &mut stopping, &text)
                .await;
            if let Some(stop) = stop {
                return stop;
            }
        }

        loop {
            let frame = tokio::select! {
                biased;
                _ = gateway_stop_signal(&mut stopping) => return Stop::Reconnect,
                code = session_lost(&mut self.watch, &self.state) => return Stop::Close(code),
                frame = receiver.next() => frame,
                _ = tick_websocket_ping(&mut ping) => {
                    if sender.write(Message::Ping(Vec::new().into())).await.is_err() {
                        return Stop::Gone;
                    }
                    continue;
                }
            };
            let text = match frame {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Ping(payload))) => {
                    if sender.write(Message::Pong(payload)).await.is_err() {
                        return Stop::Gone;
                    }
                    continue;
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return Stop::Gone,
                Some(Ok(_)) => continue,
            };
            let stop = self
                .on_frame(sender, receiver, &mut ping, &mut stopping, &text)
                .await;
            if let Some(stop) = stop {
                return stop;
            }
        }
    }

    /// Answer one client frame between turns. `Some` ends the socket.
    async fn on_frame(
        &mut self,
        sender: &mut Sender,
        receiver: &mut Receiver,
        ping: &mut Option<tokio::time::Interval>,
        stopping: &mut watch::Receiver<bool>,
        text: &str,
    ) -> Option<Stop> {
        // Between turns, every frame is checked against the session as a
        // request would be, before anything is done with it: a lost grant is
        // seen here, not at the next periodic recheck.
        if let Err(code) =
            crate::dashboard_session::session_admits_now(&mut self.watch, &self.state).await
        {
            return Some(Stop::Close(code));
        }
        let parsed: Value = match serde_json::from_str(text) {
            Ok(parsed) => parsed,
            Err(e) => {
                let error = json!({
                    "type": "error",
                    "message": format!("Invalid JSON: {e}"),
                    "code": "INVALID_JSON",
                });
                return refusal(sender, &error).await;
            }
        };
        match parsed["type"].as_str().unwrap_or("") {
            "approval_response" => {
                if is_sop_approval(&parsed) {
                    return refusal(sender, &sop_approval_refusal()).await;
                }
                let Some((request_id, decision)) = approval_decision(&parsed) else {
                    let error = json!({
                        "type": "error",
                        "message": "approval_response requires request_id and decision in {approve,deny,always}",
                        "code": "INVALID_APPROVAL_RESPONSE",
                    });
                    return refusal(sender, &error).await;
                };
                self.approve(request_id, decision).await;
                None
            }
            "message" => {
                let content = parsed["content"].as_str().unwrap_or("");
                if content.is_empty() {
                    let error = json!({
                        "type": "error",
                        "message": "Message content cannot be empty",
                        "code": "EMPTY_CONTENT",
                    });
                    return refusal(sender, &error).await;
                }
                self.turn(sender, receiver, ping, stopping, content).await
            }
            other => {
                let error = json!({
                    "type": "error",
                    "message": format!(
                        "Unsupported message type \"{other}\". Send {{\"type\":\"message\",\"content\":\"your text\"}}"
                    ),
                    "code": "UNKNOWN_MESSAGE_TYPE",
                });
                refusal(sender, &error).await
            }
        }
    }

    /// Run one turn. A connection whose binding lapsed before the turn ran
    /// is replaced once, and the conversation rebound on the new one.
    async fn turn(
        &mut self,
        sender: &mut Sender,
        receiver: &mut Receiver,
        ping: &mut Option<tokio::time::Interval>,
        stopping: &mut watch::Receiver<bool>,
        content: &str,
    ) -> Option<Stop> {
        let mut renewed = false;
        loop {
            // A new handshake may have replaced the chat's connection since
            // the conversation was bound; rebind it there first, so its
            // approval requests reach this socket.
            match self.core.rebind().await {
                Ok(()) => {}
                Err(CoreFailure::NotFound) => return conversation_closed(sender).await,
                Err(failure) => return turn_failed(sender, failure).await,
            }
            match self
                .relay_turn(sender, receiver, ping, stopping, content)
                .await
            {
                TurnEnd::Finished => return None,
                TurnEnd::Stop(stop) => return Some(stop),
                TurnEnd::AuthRequired if !renewed => {
                    renewed = true;
                    if let Err(failure) = self.core.renew().await {
                        return turn_failed(sender, failure).await;
                    }
                }
                TurnEnd::AuthRequired => {
                    self.core.end_session();
                    return turn_failed(sender, CoreFailure::SessionEnded).await;
                }
            }
        }
    }

    /// Prompt the core and relay the turn to the socket.
    ///
    /// A client that leaves mid-turn, or whose session ends, ends the chat
    /// there: the approvals it was shown are refused, and the close that
    /// follows cancels the turn.
    async fn relay_turn(
        &mut self,
        sender: &mut Sender,
        receiver: &mut Receiver,
        ping: &mut Option<tokio::time::Interval>,
        stopping: &mut watch::Receiver<bool>,
        content: &str,
    ) -> TurnEnd {
        let client = self.core.bound();
        let session_id = self.core.core_id.clone();
        // A fresh receiver sees only what follows this turn's prompt, and the
        // turn's identity tells its end apart from another viewer's turn on
        // the same conversation.
        let mut updates = client.notifications();
        let generation = uuid::Uuid::new_v4().as_u64_pair().0;
        let prompt = client.request_with_timeout(
            Method::SessionPrompt.wire_name(),
            json!({
                "session_id": session_id,
                "prompt": content,
                "client_turn_generation": generation,
            }),
            TURN_CEILING,
        );
        tokio::pin!(prompt);
        let mut turn = TurnRelay {
            client: Arc::clone(&client),
            session_id: session_id.clone(),
            generation,
            client_gone: false,
            relayed_any: false,
            stop: None,
            meter: ContextMeter::default(),
            approval_receipts: BTreeSet::new(),
        };

        loop {
            // The chat ends with its browser or its session, mid-turn too:
            // the close that follows cancels the turn.
            if turn.client_gone {
                return turn.end();
            }
            tokio::select! {
                biased;
                update = updates.recv() => match update {
                    Ok(note) => {
                        if turn.relay(sender, note).await == Relayed::TurnEnded {
                            return turn.end();
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_attrs(json!({"session_id": session_id, "missed": missed})),
                            "dashboard chat fell behind the core's session stream; frames were lost"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        turn.fail(sender, "the connection to the agent runtime was lost; the turn ended", "CORE_UNAVAILABLE").await;
                        return turn.end();
                    }
                },
                answered = &mut prompt => {
                    // Every frame of the turn reached the connection before
                    // its answer did; take whatever this receiver has not
                    // read yet.
                    loop {
                        match updates.try_recv() {
                            Ok(note) => {
                                if turn.relay(sender, note).await == Relayed::TurnEnded {
                                    return turn.end();
                                }
                            }
                            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                            Err(_) => break,
                        }
                    }
                    match answered {
                        // The per-operation check refused the prompt before
                        // its turn ran: present the credential again.
                        Err(ClientError::Rpc(error))
                            if error.code == AUTH_REQUIRED && !turn.relayed_any =>
                        {
                            return TurnEnd::AuthRequired;
                        }
                        // A prompt refused before its turn ran has no
                        // terminal event; this answer is the whole outcome.
                        Err(error) => match self.core.failure(error) {
                            CoreFailure::SessionEnded => {
                                turn.stop = Some(Stop::Close(WS_CLOSE_SESSION_ENDED));
                            }
                            // The core no longer has the conversation.
                            CoreFailure::NotFound => {
                                turn.fail(sender, CONVERSATION_CLOSED_MESSAGE, "CONVERSATION_CLOSED")
                                    .await;
                                turn.stop = Some(Stop::Close(WS_CLOSE_CONVERSATION_CLOSED));
                            }
                            failure => {
                                let (message, code) = failure_frame(&failure);
                                turn.fail(sender, &message, code).await;
                            }
                        },
                        // The turn ended, but its terminal event was lost.
                        Ok(_) => {
                            turn.fail(
                                sender,
                                "the turn finished, but its result was not received; reload the session to see it",
                                "TURN_RESULT_LOST",
                            )
                            .await;
                        }
                    }
                    return turn.end();
                },
                code = session_lost(&mut self.watch, &self.state), if !turn.client_gone => {
                    // The chat ends here: the approvals it was shown are
                    // refused, and its close cancels the turn.
                    turn.stop = Some(Stop::Close(code));
                    turn.disconnect().await;
                },
                _ = gateway_stop_signal(stopping), if !turn.client_gone => {
                    turn.stop = Some(Stop::Reconnect);
                    turn.disconnect().await;
                },
                frame = receiver.next(), if !turn.client_gone => {
                    let text = match frame {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Ping(payload))) => {
                            if sender.write(Message::Pong(payload)).await.is_err() {
                                turn.disconnect().await;
                            }
                            continue;
                        }
                        Some(Ok(Message::Pong(_) | Message::Binary(_))) => continue,
                        Some(Ok(Message::Close(_)) | Err(_)) | None => {
                            turn.disconnect().await;
                            continue;
                        }
                    };
                    // Mid-turn, the same check before acting on a frame. On a
                    // loss the chat ends, as when the session arm fires.
                    if let Err(code) =
                        crate::dashboard_session::session_admits_now(&mut self.watch, &self.state)
                            .await
                    {
                        turn.stop = Some(Stop::Close(code));
                        turn.disconnect().await;
                        continue;
                    }
                    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
                        let error = json!({
                            "type": "error",
                            "message": "Invalid JSON. Send {\"type\":\"message\",\"content\":\"your text\"}",
                            "code": "INVALID_JSON",
                        });
                        if send(sender, &error).await.is_err() {
                            turn.disconnect().await;
                        }
                        continue;
                    };
                    match parsed["type"].as_str() {
                        Some("approval_response") => {
                            if is_sop_approval(&parsed) {
                                if send(sender, &sop_approval_refusal()).await.is_err() {
                                    turn.disconnect().await;
                                }
                            } else if let Some((request_id, decision)) = approval_decision(&parsed) {
                                turn.approval_receipts.remove(request_id);
                                self.approve(request_id, decision).await;
                            }
                        }
                        Some("message") => {
                            let error = json!({
                                "type": "error",
                                "message": "a running turn can't be steered from this chat yet; send the message when the turn ends",
                                "code": "CAPABILITY_MISSING",
                            });
                            if send(sender, &error).await.is_err() {
                                turn.disconnect().await;
                            }
                        }
                        _ => {}
                    }
                },
                _ = tick_websocket_ping(ping), if !turn.client_gone => {
                    if sender.write(Message::Ping(Vec::new().into())).await.is_err() {
                        turn.disconnect().await;
                    }
                },
            }
        }
    }

    // `&mut self`: a shared borrow held across the call would need `Chat:
    // Sync`, which the session watch's pending recheck is not.
    async fn approve(&mut self, request_id: &str, decision: &str) {
        let session_id = self.core.core_id.clone();
        let answer = json!({
            "session_id": session_id,
            "request_id": request_id,
            "decision": decision,
        });
        if let Err(failure) = self
            .core
            .call::<Value>(Method::SessionApprove, answer)
            .await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(json!({
                        "session_id": session_id,
                        "request_id": request_id,
                        "error": format!("{failure:?}"),
                    })),
                "dashboard chat approval was not delivered to the core"
            );
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Relayed {
    Continue,
    TurnEnded,
}

/// One turn's relay from the core's session stream to the socket.
struct TurnRelay {
    client: Arc<RpcClient>,
    session_id: String,
    generation: u64,
    client_gone: bool,
    /// Whether any of the turn's frames reached this relay.
    relayed_any: bool,
    /// How the socket ends once the turn has.
    stop: Option<Stop>,
    meter: ContextMeter,
    /// Approval requests shown on this socket and not yet answered.
    approval_receipts: BTreeSet<String>,
}

impl TurnRelay {
    fn end(self) -> TurnEnd {
        match self.stop {
            Some(stop) => TurnEnd::Stop(stop),
            None if self.client_gone => TurnEnd::Stop(Stop::Gone),
            None => TurnEnd::Finished,
        }
    }

    /// The client left: nobody can answer the approvals it was shown, so
    /// they are refused rather than left to time out.
    async fn disconnect(&mut self) {
        self.client_gone = true;
        for request_id in std::mem::take(&mut self.approval_receipts) {
            refuse_unanswered(&self.client, &self.session_id, &request_id).await;
        }
    }

    async fn relay(&mut self, sender: &mut Sender, note: Notification) -> Relayed {
        if note.method != SESSION_UPDATE {
            return Relayed::Continue;
        }
        let Ok(event) = serde_json::from_value::<SessionUpdateEvent>(note.params) else {
            return Relayed::Continue;
        };
        if event_session(&event) != self.session_id {
            return Relayed::Continue;
        }
        self.relayed_any = true;
        let frame = match event {
            SessionUpdateEvent::TurnComplete {
                outcome,
                content,
                client_turn_generation,
                usage,
                ..
            } => {
                if client_turn_generation.is_some_and(|other| other != self.generation) {
                    return Relayed::Continue;
                }
                let frame = match outcome {
                    TurnCompletionOutcome::Completed => {
                        self.meter.done_frame(&content, usage.as_deref())
                    }
                    TurnCompletionOutcome::Cancelled => json!({ "type": "aborted" }),
                    TurnCompletionOutcome::Failed => json!({
                        "type": "error",
                        "message": "The agent could not finish this turn.",
                        "code": "AGENT_ERROR",
                    }),
                };
                if !self.client_gone {
                    let _ = send(sender, &frame).await;
                }
                return Relayed::TurnEnded;
            }
            SessionUpdateEvent::ApprovalRequest {
                request_id,
                tool_name,
                arguments_summary,
                timeout_secs,
                ..
            } => {
                if self.client_gone {
                    refuse_unanswered(&self.client, &self.session_id, &request_id).await;
                    return Relayed::Continue;
                }
                let frame = json!({
                    "type": "approval_request",
                    "request_id": request_id,
                    "tool": tool_name,
                    "arguments_summary": arguments_summary,
                    "timeout_secs": timeout_secs,
                });
                self.approval_receipts.insert(request_id);
                if send(sender, &frame).await.is_err() {
                    self.disconnect().await;
                }
                return Relayed::Continue;
            }
            SessionUpdateEvent::ContextUsage {
                input_tokens,
                max_context_tokens,
                model_context_window,
                ..
            } => {
                self.meter
                    .observe(input_tokens, max_context_tokens, model_context_window);
                return Relayed::Continue;
            }
            SessionUpdateEvent::BudgetWarning { .. } => return Relayed::Continue,
            SessionUpdateEvent::AgentMessageChunk { text, .. } => {
                json!({ "type": "chunk", "content": text })
            }
            SessionUpdateEvent::AgentThoughtChunk { text, .. } => {
                json!({ "type": "thinking", "content": text })
            }
            SessionUpdateEvent::ToolCall {
                tool_call_id,
                name,
                raw_input,
                ..
            } => {
                json!({ "type": "tool_call", "id": tool_call_id, "name": name, "args": raw_input })
            }
            SessionUpdateEvent::ToolResult {
                tool_call_id,
                name,
                raw_output,
                ..
            } => {
                json!({ "type": "tool_result", "id": tool_call_id, "name": name, "output": raw_output })
            }
            SessionUpdateEvent::Plan { entries, .. } => {
                json!({ "type": "plan", "entries": entries })
            }
            SessionUpdateEvent::HistoryTrimmed {
                dropped_messages,
                dropped_turns,
                kept_turns,
                reason,
                token_budget,
                tokens_before,
                tokens_after,
                tokens_before_source,
                tokens_after_source,
                unsatisfiable_floor,
                ..
            } => history_trimmed_ws_frame(
                dropped_messages,
                dropped_turns,
                kept_turns,
                &reason,
                token_budget,
                tokens_before,
                tokens_after,
                tokens_before_source.map(|s| s.as_str()),
                tokens_after_source.map(|s| s.as_str()),
                unsatisfiable_floor,
            ),
        };
        if !self.client_gone && send(sender, &frame).await.is_err() {
            self.disconnect().await;
        }
        Relayed::Continue
    }

    async fn fail(&mut self, sender: &mut Sender, message: &str, code: &str) {
        if self.client_gone {
            return;
        }
        let error = json!({ "type": "error", "message": message, "code": code });
        if send(sender, &error).await.is_err() {
            self.disconnect().await;
        }
    }
}

fn event_session(event: &SessionUpdateEvent) -> &str {
    match event {
        SessionUpdateEvent::AgentMessageChunk { session_id, .. }
        | SessionUpdateEvent::AgentThoughtChunk { session_id, .. }
        | SessionUpdateEvent::ToolCall { session_id, .. }
        | SessionUpdateEvent::ToolResult { session_id, .. }
        | SessionUpdateEvent::ApprovalRequest { session_id, .. }
        | SessionUpdateEvent::ContextUsage { session_id, .. }
        | SessionUpdateEvent::Plan { session_id, .. }
        | SessionUpdateEvent::TurnComplete { session_id, .. }
        | SessionUpdateEvent::BudgetWarning { session_id, .. }
        | SessionUpdateEvent::HistoryTrimmed { session_id, .. } => session_id,
    }
}

/// What the core reported about the context during the turn, for the `done`
/// frame's context meter when the turn's usage does not say.
#[derive(Default)]
struct ContextMeter {
    last_input_tokens: Option<u64>,
    max_context_tokens: Option<u64>,
    model_context_window: Option<u64>,
}

impl ContextMeter {
    fn observe(
        &mut self,
        input_tokens: Option<u64>,
        max_context_tokens: Option<u64>,
        model_context_window: Option<u64>,
    ) {
        self.last_input_tokens = input_tokens.or(self.last_input_tokens);
        self.max_context_tokens = max_context_tokens.or(self.max_context_tokens);
        self.model_context_window = model_context_window.or(self.model_context_window);
    }

    /// The in-process chat's `done` frame, from the turn's usage as the
    /// core reports it. What the core did not report is `null`.
    fn done_frame(&self, full_response: &str, usage: Option<&TurnUsageTotals>) -> Value {
        let by_provider: Vec<Value> = usage
            .map(|usage| {
                usage
                    .usage_by_provider
                    .iter()
                    .map(|entry| {
                        json!({
                            "provider_ref": entry.provider_ref,
                            "model": entry.model,
                            "input_tokens": entry.input_tokens,
                            "output_tokens": entry.output_tokens,
                            "cached_input_tokens": entry.cached_input_tokens,
                            "cost_usd": entry.cost_usd,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let provider_ref = usage.and_then(|usage| usage.provider_ref.clone());
        let model = usage.and_then(|usage| usage.model.clone());
        let mut done = json!({
            "type": "done",
            "full_response": full_response,
            "input_tokens": usage.and_then(|usage| usage.input_tokens),
            "output_tokens": usage.and_then(|usage| usage.output_tokens),
            "tokens_used": usage.and_then(|usage| usage.tokens_used),
            "cost_usd": usage.and_then(|usage| usage.cost_usd),
            "model": model,
            "provider": provider_ref,
            "provider_ref": provider_ref,
            "max_context_tokens": usage
                .and_then(|usage| usage.max_context_tokens)
                .or(self.max_context_tokens),
            "last_input_tokens": usage
                .and_then(|usage| usage.last_input_tokens)
                .or(self.last_input_tokens),
            "last_serving_provider_ref": provider_ref,
            "last_serving_model": model,
            "usage_by_provider": by_provider,
        });
        if let Some(window) = usage
            .and_then(|usage| usage.model_context_window)
            .or(self.model_context_window)
        {
            done["model_context_window"] = Value::from(window);
        }
        done
    }
}

fn is_sop_approval(parsed: &Value) -> bool {
    parsed["kind"].as_str() == Some("sop")
}

fn sop_approval_refusal() -> Value {
    json!({
        "type": "error",
        "message": "SOP approvals can't be recorded from this chat yet; approve the run from an operator session",
        "code": "CAPABILITY_MISSING",
    })
}

/// The tool-approval answer in an `approval_response` frame, as the core's
/// decision name, or `None` when the frame lacks a request or a known
/// decision.
fn approval_decision(parsed: &Value) -> Option<(&str, &'static str)> {
    let request_id = parsed["request_id"].as_str().filter(|id| !id.is_empty())?;
    let decision = match parsed["decision"].as_str()? {
        "approve" => "allow_once",
        "always" => "allow_always",
        "deny" => "reject",
        _ => return None,
    };
    Some((request_id, decision))
}

/// Refuse an approval request whose viewer left.
async fn refuse_unanswered(client: &RpcClient, session_id: &str, request_id: &str) {
    let answer = json!({
        "session_id": session_id,
        "request_id": request_id,
        "decision": "reject",
    });
    if let Err(error) = client.request(Method::SessionApprove, answer).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                json!({
                    "session_id": session_id,
                    "request_id": request_id,
                    "error": error.to_string(),
                })
            ),
            "approval left unanswered by a closed dashboard chat was not refused"
        );
    }
}

/// The error frame's text and code for a failure that does not end the
/// socket.
fn failure_frame(failure: &CoreFailure) -> (String, &'static str) {
    match failure {
        CoreFailure::Forbidden(message) => (message.clone(), "NOT_PERMITTED"),
        CoreFailure::NotFound => ("session not found".into(), "SESSION_NOT_FOUND"),
        CoreFailure::Refused(message) => (message.clone(), "CORE_REFUSED"),
        CoreFailure::Unavailable | CoreFailure::Lost(_) | CoreFailure::SessionEnded => (
            "the agent runtime is not reachable; try again".into(),
            "CORE_UNAVAILABLE",
        ),
        CoreFailure::ConversationLimit => (
            crate::dashboard_core::conversation_limit_message(),
            "CONVERSATION_LIMIT",
        ),
    }
}

/// Report a failure before a turn ran: the socket closes when the session
/// has ended, and otherwise stays open with an error frame.
async fn turn_failed(sender: &mut Sender, failure: CoreFailure) -> Option<Stop> {
    if matches!(failure, CoreFailure::SessionEnded) {
        return Some(Stop::Close(WS_CLOSE_SESSION_ENDED));
    }
    let limit = matches!(failure, CoreFailure::ConversationLimit);
    let (message, code) = failure_frame(&failure);
    let error = json!({ "type": "error", "message": message, "code": code });
    let stop = refusal(sender, &error).await;
    if limit {
        return stop.or(Some(Stop::Close(WS_CLOSE_CONVERSATION_LIMIT)));
    }
    stop
}

/// The conversation was closed under this socket: say so and close.
async fn conversation_closed(sender: &mut Sender) -> Option<Stop> {
    let error = json!({
        "type": "error",
        "message": CONVERSATION_CLOSED_MESSAGE,
        "code": "CONVERSATION_CLOSED",
    });
    refusal(sender, &error)
        .await
        .or(Some(Stop::Close(WS_CLOSE_CONVERSATION_CLOSED)))
}

/// Send an error frame; the socket ends only when the peer is gone.
async fn refusal(sender: &mut Sender, frame: &Value) -> Option<Stop> {
    send(sender, frame).await.err().map(|_| Stop::Gone)
}

async fn finish(sender: &mut Sender, receiver: &mut Receiver, stop: Stop) {
    match stop {
        Stop::Gone => {}
        Stop::Reconnect => {
            close_for_reconnect(&mut sender.sink, receiver, "gateway restarting").await;
        }
        Stop::Close(code) => {
            let reason = match code {
                WS_CLOSE_CONVERSATION_LIMIT => "conversation limit reached",
                WS_CLOSE_CONVERSATION_CLOSED => "conversation closed",
                _ => SESSION_ENDED_REASON,
            };
            close_with(&mut sender.sink, receiver, code, reason).await;
        }
    }
}

async fn send(sender: &mut Sender, frame: &Value) -> Result<(), axum::Error> {
    sender.write(Message::Text(frame.to_string().into())).await
}
