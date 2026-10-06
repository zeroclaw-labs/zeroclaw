//! The dashboard chat socket, `GET /ws/chat`, and its stop button,
//! `POST /api/sessions/{id}/abort`, served through the core for the separate
//! `zeroclaw-gw`. The gateway translates the chat frame protocol to the
//! core's session methods and their `session/update` stream. The core runs
//! every turn and keeps the session; the gateway holds nothing beyond the
//! socket.
//!
//! What the core does not offer yet is refused with a frame that says so,
//! never approximated: steering a running turn, and SOP approvals, whose
//! decision the core would record under the wrong approver.

use std::collections::BTreeSet;
use std::time::Duration;

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use zeroclaw_api::jsonrpc::error_codes::SESSION_NOT_FOUND;
use zeroclaw_rpc_client::{Method, Notification};
use zeroclaw_rpc_proto::notification::SESSION_UPDATE;
use zeroclaw_rpc_proto::types::{
    SessionCancelResult, SessionNewResult, SessionUpdateEvent, TurnCompletionOutcome,
};

use crate::core_rpc::{CoreCall, CoreError};
use crate::ws::{
    ConnectParams, WS_PROTOCOL, WsQuery, first_chat_message_content, history_trimmed_ws_frame,
};

/// The longest the gateway waits for a turn's answer. The core's own limits
/// end a turn long before this.
const TURN_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the socket is pinged: the in-process gateway's default.
const PING_INTERVAL: Duration = Duration::from_secs(30);

type Sender = SplitSink<WebSocket, Message>;
type Receiver = SplitStream<WebSocket>;

/// Open the chat socket on the caller's core connection. The session is
/// created, or resumed, before the upgrade, so a missing agent or a refused
/// session is answered over HTTP, as on the in-process gateway.
pub(crate) async fn ws_chat_through_core(
    core: CoreCall,
    params: WsQuery,
    headers: &HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, CoreError> {
    require_chat_features(&core)?;
    let Some(agent_alias) = params.agent_alias.filter(|s| !s.trim().is_empty()) else {
        return Ok((
            StatusCode::BAD_REQUEST,
            "Missing required `agent` query parameter — pass `?agent=<alias>` matching a configured [agents.<alias>] entry.",
        )
            .into_response());
    };
    let session_id = params
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let cwd = params.cwd.or(params.workspace_dir);
    // One core connection carries every chat of this credential, so a new
    // chat must not evict the credential's other idle sessions.
    let mut request = json!({
        "agent_alias": agent_alias,
        "session_id": session_id,
        "keep_siblings": true,
    });
    if let Some(cwd) = &cwd {
        request["cwd"] = cwd.clone().into();
    }
    let opened: SessionNewResult = core.call(Method::SessionNew, request).await?;
    let requests_protocol = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|protos| protos.split(',').any(|p| p.trim() == WS_PROTOCOL));
    let ws = if requests_protocol {
        ws.protocols([WS_PROTOCOL])
    } else {
        ws
    };
    Ok(ws
        .on_upgrade(move |socket| chat(socket, core, session_id, cwd, opened.message_count))
        .into_response())
}

/// `POST /api/sessions/{id}/abort` through the core: cancel the session's
/// running turn. As on the in-process gateway, a session with no running turn
/// answers `no_active_response`.
#[derive(Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AbortAddress {
    #[default]
    Auto,
    Raw,
    Key,
}

pub(crate) async fn abort_through_core(
    core: &CoreCall,
    id: &str,
    address: AbortAddress,
) -> Result<Response, CoreError> {
    if !core
        .core_features()
        .iter()
        .any(|f| f == zeroclaw_rpc_proto::feature::SESSION_CANCEL_CHAT_KEY)
    {
        return Err(CoreError::MissingFeature(
            zeroclaw_rpc_proto::feature::SESSION_CANCEL_CHAT_KEY,
        ));
    }
    let exact = matches!(address, AbortAddress::Key)
        || matches!(address, AbortAddress::Auto)
            && (id.starts_with("rpc_") || id.starts_with("gw_"));
    let params = if exact {
        json!({"session_id": id, "session_key": id})
    } else {
        json!({"session_id": id})
    };
    let status = match core
        .call::<SessionCancelResult>(Method::SessionCancel, params)
        .await
    {
        Ok(result) if result.cancelled => "aborted",
        Ok(_) => "no_active_response",
        Err(CoreError::Rpc(error)) if error.code == SESSION_NOT_FOUND => "no_active_response",
        Err(error) => return Err(error),
    };
    Ok(Json(json!({ "status": status })).into_response())
}

async fn chat(
    socket: WebSocket,
    core: CoreCall,
    session_id: String,
    cwd: Option<String>,
    message_count: usize,
) {
    let (mut sender, mut receiver) = socket.split();
    let start = json!({
        "type": "session_start",
        "session_id": session_id,
        "resumed": message_count > 0,
        "message_count": message_count,
    });
    if send(&mut sender, &start).await.is_err() {
        return;
    }
    let mut ping = ping_interval();

    // The first frame may be a `connect` handshake; anything else is the
    // first message.
    let mut first_message = None;
    loop {
        let frame = tokio::select! {
            frame = receiver.next() => frame,
            _ = ping.tick() => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
                continue;
            }
        };
        match frame {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<ConnectParams>(&text) {
                    Ok(connect) if connect.msg_type == "connect" => {
                        // The session was bound to its working directory when
                        // it was opened, before this frame could arrive.
                        if connect.cwd.is_some() && connect.cwd != cwd {
                            let error = json!({
                                "type": "error",
                                "message": "this gateway fixes the session's working directory when the socket opens: pass `cwd` in the URL",
                                "code": "INVALID_CWD",
                            });
                            let _ = send(&mut sender, &error).await;
                            return;
                        }
                        let ack = json!({
                            "type": "connected",
                            "message": "Connection established",
                        });
                        let _ = send(&mut sender, &ack).await;
                    }
                    _ => first_message = Some(text.to_string()),
                }
                break;
            }
            Some(Ok(Message::Ping(payload))) => {
                if sender.send(Message::Pong(payload)).await.is_err() {
                    return;
                }
            }
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(_)) | Err(_)) | None => return,
            Some(Ok(_)) => {}
        }
    }

    if let Some(text) = first_message {
        match serde_json::from_str::<Value>(&text) {
            Ok(parsed) if parsed["type"].as_str() == Some("message") => {
                if let Some(content) = first_chat_message_content(&text)
                    && run_turn(
                        &core,
                        &mut sender,
                        &mut receiver,
                        &mut ping,
                        &session_id,
                        &content,
                    )
                    .await
                {
                    return;
                }
            }
            Ok(parsed) => {
                let unknown = parsed["type"].as_str().unwrap_or("unknown");
                let error = json!({
                    "type": "error",
                    "message": format!(
                        "Unsupported message type \"{unknown}\". Send {{\"type\":\"message\",\"content\":\"your text\"}}"
                    ),
                });
                let _ = send(&mut sender, &error).await;
            }
            Err(_) => {
                let error = json!({
                    "type": "error",
                    "message": "Invalid JSON. Send {\"type\":\"message\",\"content\":\"your text\"}",
                });
                let _ = send(&mut sender, &error).await;
            }
        }
    }

    loop {
        tokio::select! {
            _ = ping.tick() => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            frame = receiver.next() => {
                let text = match frame {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Ping(payload))) => {
                        if sender.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Some(Ok(Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                };
                let parsed: Value = match serde_json::from_str(&text) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        let error = json!({
                            "type": "error",
                            "message": format!("Invalid JSON: {e}"),
                            "code": "INVALID_JSON",
                        });
                        let _ = send(&mut sender, &error).await;
                        continue;
                    }
                };
                let msg_type = parsed["type"].as_str().unwrap_or("");
                if msg_type == "approval_response" {
                    if is_sop_approval(&parsed) {
                        let _ = send(&mut sender, &sop_approval_refusal()).await;
                        continue;
                    }
                    let Some((request_id, decision)) = approval_decision(&parsed) else {
                        let error = json!({
                            "type": "error",
                            "message": "approval_response requires request_id and decision in {approve,deny,always}",
                            "code": "INVALID_APPROVAL_RESPONSE",
                        });
                        let _ = send(&mut sender, &error).await;
                        continue;
                    };
                    approve(&core, &session_id, request_id, decision).await;
                    continue;
                }
                if msg_type != "message" {
                    let error = json!({
                        "type": "error",
                        "message": format!(
                            "Unsupported message type \"{msg_type}\". Send {{\"type\":\"message\",\"content\":\"your text\"}}"
                        ),
                        "code": "UNKNOWN_MESSAGE_TYPE",
                    });
                    let _ = send(&mut sender, &error).await;
                    continue;
                }
                let content = parsed["content"].as_str().unwrap_or("").to_string();
                if content.is_empty() {
                    let error = json!({
                        "type": "error",
                        "message": "Message content cannot be empty",
                        "code": "EMPTY_CONTENT",
                    });
                    let _ = send(&mut sender, &error).await;
                    continue;
                }
                if run_turn(&core, &mut sender, &mut receiver, &mut ping, &session_id, &content).await {
                    break;
                }
            }
        }
    }
}

/// Run one turn on the core and relay it. Returns `true` once the client has
/// gone.
///
/// The turn belongs to the core connection it was started on, not to the
/// socket: a client that leaves mid-turn does not stop it. The socket keeps
/// holding the connection until the turn ends, and answers any approval the
/// turn asks for after that with a rejection, as the in-process socket does
/// when nobody is left to answer.
async fn run_turn(
    core: &CoreCall,
    sender: &mut Sender,
    receiver: &mut Receiver,
    ping: &mut tokio::time::Interval,
    session_id: &str,
    content: &str,
) -> bool {
    // A fresh receiver sees only what follows this turn's prompt, and the
    // turn's identity tells its end apart from another viewer's turn on the
    // same session.
    let mut updates = core.notifications();
    let generation = uuid::Uuid::new_v4().as_u64_pair().0;
    let prompt = core.request_within(
        Method::SessionPrompt,
        json!({
            "session_id": session_id,
            "prompt": content,
            "client_turn_generation": generation,
        }),
        TURN_CEILING,
    );
    tokio::pin!(prompt);
    let mut turn = TurnRelay {
        core,
        session_id,
        generation,
        client_gone: false,
        meter: ContextMeter::default(),
        approval_receipts: BTreeSet::new(),
    };

    loop {
        tokio::select! {
            biased;
            update = updates.recv() => match update {
                Ok(note) => {
                    if turn.relay(sender, note).await == Relayed::TurnEnded {
                        return turn.client_gone;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(json!({"session_id": session_id, "missed": missed})),
                        "chat socket fell behind the core's session stream; frames were lost"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    turn.fail(sender, "the core connection was lost; the turn ended", "CORE_UNAVAILABLE").await;
                    return turn.client_gone;
                }
            },
            answered = &mut prompt => {
                // Every frame of the turn reached the connection before its
                // answer did; take whatever this receiver has not read yet.
                loop {
                    match updates.try_recv() {
                        Ok(note) => {
                            if turn.relay(sender, note).await == Relayed::TurnEnded {
                                return turn.client_gone;
                            }
                        }
                        Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                        Err(_) => break,
                    }
                }
                match answered {
                    // A prompt refused before its turn ran has no terminal
                    // event; this answer is the whole outcome.
                    Err(error) => {
                        let (_, code) = error.status();
                        turn.fail(sender, &core_error_message(&error), &code.to_uppercase()).await;
                    }
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
                return turn.client_gone;
            },
            frame = receiver.next(), if !turn.client_gone => {
                let text = match frame {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Ping(payload))) => {
                        if sender.send(Message::Pong(payload)).await.is_err() {
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
                let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
                    let error = json!({
                        "type": "error",
                        "message": "Invalid JSON. Send {\"type\":\"message\",\"content\":\"your text\"}",
                        "code": "INVALID_JSON",
                    });
                    if send(sender, &error).await.is_err() { turn.disconnect().await; }
                    continue;
                };
                match parsed["type"].as_str() {
                    Some("approval_response") => {
                        if is_sop_approval(&parsed) {
                            if send(sender, &sop_approval_refusal()).await.is_err() { turn.disconnect().await; }
                        } else if let Some((request_id, decision)) = approval_decision(&parsed) {
                            approve(core, session_id, request_id, decision).await;
                        }
                    }
                    Some("message") => {
                        let error = json!({
                            "type": "error",
                            "message": "this gateway cannot steer a running turn yet; send the message when the turn ends",
                            "code": "CAPABILITY_MISSING",
                        });
                        if send(sender, &error).await.is_err() { turn.disconnect().await; }
                    }
                    _ => {}
                }
            },
            _ = ping.tick(), if !turn.client_gone => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    turn.disconnect().await;
                }
            },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Relayed {
    Continue,
    TurnEnded,
}

/// One turn's relay from the core's session stream to the socket.
struct TurnRelay<'a> {
    core: &'a CoreCall,
    session_id: &'a str,
    generation: u64,
    client_gone: bool,
    meter: ContextMeter,
    // Delivered request identities for this socket/turn, not pending approval state.
    approval_receipts: BTreeSet<String>,
}

impl TurnRelay<'_> {
    async fn disconnect(&mut self) {
        self.client_gone = true;
        for request_id in std::mem::take(&mut self.approval_receipts) {
            unreachable(self.core, self.session_id, &request_id, self.generation).await;
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
        let frame = match event {
            SessionUpdateEvent::TurnComplete {
                outcome,
                content,
                client_turn_generation,
                error_code,
                error_message,
                ..
            } => {
                if client_turn_generation.is_some_and(|other| other != self.generation) {
                    return Relayed::Continue;
                }
                let frame = match outcome {
                    TurnCompletionOutcome::Completed => self.meter.done_frame(&content),
                    TurnCompletionOutcome::Cancelled => json!({ "type": "aborted" }),
                    TurnCompletionOutcome::Failed => json!({
                        "type": "error",
                        "message": error_message.unwrap_or(content),
                        "code": error_code.as_deref().unwrap_or("AGENT_ERROR"),
                    }),
                };
                if !self.client_gone {
                    let _ = send(sender, &frame).await;
                }
                return Relayed::TurnEnded;
            }
            SessionUpdateEvent::ApprovalRequest {
                client_turn_generation,
                request_id,
                tool_name,
                arguments_summary,
                timeout_secs,
                ..
            } => {
                if client_turn_generation != Some(self.generation) {
                    return Relayed::Continue;
                }
                if self.client_gone {
                    unreachable(self.core, self.session_id, &request_id, self.generation).await;
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
        | SessionUpdateEvent::HistoryTrimmed { session_id, .. } => session_id,
    }
}

/// What the core reported about the context during the turn, for the `done`
/// frame's context meter.
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

    /// The in-process gateway's `done` frame, with what the core's turn
    /// stream carries. Token totals, cost and the serving model are not on
    /// that stream yet and are `null`.
    fn done_frame(&self, full_response: &str) -> Value {
        let mut done = json!({
            "type": "done",
            "full_response": full_response,
            "input_tokens": null,
            "output_tokens": null,
            "tokens_used": null,
            "cost_usd": null,
            "model": null,
            "provider": null,
            "provider_ref": null,
            "max_context_tokens": self.max_context_tokens,
            "last_input_tokens": self.last_input_tokens,
            "last_serving_provider_ref": null,
            "last_serving_model": null,
            "usage_by_provider": [],
        });
        if let Some(window) = self.model_context_window {
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
        "message": "this gateway cannot record SOP approvals yet; approve the run from the core's own tools",
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

fn require_chat_features(core: &CoreCall) -> Result<(), CoreError> {
    let required = [
        zeroclaw_rpc_proto::feature::SESSION_NEW_VALIDATES_AGENT,
        zeroclaw_rpc_proto::feature::SESSION_TURN_ERRORS,
        zeroclaw_rpc_proto::feature::SESSION_APPROVAL_UNREACHABLE,
    ];
    for feature in required {
        if !core
            .core_features()
            .iter()
            .any(|advertised| advertised == feature)
        {
            return Err(CoreError::MissingFeature(feature));
        }
    }
    Ok(())
}

async fn unreachable(core: &CoreCall, session_id: &str, request_id: &str, generation: u64) {
    let answer = json!({
        "session_id": session_id, "request_id": request_id,
        "decision": "unreachable", "client_turn_generation": generation,
    });
    if let Err(error) = core.request(Method::SessionApprove, answer).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(json!({ "session_id": session_id, "request_id": request_id, "error": format!("{error:?}") })),
            "chat approval viewer cleanup was refused"
        );
    }
}

async fn approve(core: &CoreCall, session_id: &str, request_id: &str, decision: &str) {
    let answer = json!({
        "session_id": session_id,
        "request_id": request_id,
        "decision": decision,
    });
    if let Err(error) = core.request(Method::SessionApprove, answer).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(json!({
                    "session_id": session_id,
                    "request_id": request_id,
                    "error": format!("{error:?}"),
                })),
            "chat approval was not delivered to the core"
        );
    }
}

fn core_error_message(error: &CoreError) -> String {
    match error {
        CoreError::AuthRequired(message)
        | CoreError::Forbidden(message)
        | CoreError::Unavailable(message)
        | CoreError::UntrustedEndpoint(message) => message.clone(),
        CoreError::Busy => "every core connection this gateway may hold is in use".into(),
        CoreError::Timeout => "the core did not answer in time".into(),
        CoreError::Rpc(error) => error.message.clone(),
        CoreError::MissingFeature(feature) => format!("the core lacks required feature {feature}"),
        CoreError::VersionMismatch { core, gateway } => {
            format!("core version {core} differs from gateway {gateway}")
        }
    }
}

fn ping_interval() -> tokio::time::Interval {
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval
}

async fn send(sender: &mut Sender, frame: &Value) -> Result<(), axum::Error> {
    sender.send(Message::Text(frame.to_string().into())).await
}
