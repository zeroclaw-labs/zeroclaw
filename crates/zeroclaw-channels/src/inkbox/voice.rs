//! Call-media WebSocket bridge (Inkbox STT/TTS duplex).
//!
//! Inkbox connects a call's audio leg to this WebSocket. We accept the upgrade
//! with the `x-use-inkbox-*: true` headers, which tell Inkbox to run its own
//! speech-to-text and text-to-speech so the leg speaks a **text** duplex:
//!
//! * inbound  — `{"event":"start"}`, then
//!   `{"event":"transcript","is_final":true,"text":"…","turn_id":"…"}`
//! * outbound — `{"event":"text","delta":"…","turn_id":"…"}` followed by
//!   `{"event":"text","done":true,"turn_id":"…"}`
//!
//! Each final caller transcript becomes a [`ChannelMessage`] tagged
//! `reply_target = "call:<conn_id>"`. The agent's reply comes back through
//! [`InkboxChannel::send`](super::InkboxChannel), which calls [`speak_to_call`]
//! to push the text onto this socket as TTS. The raw-audio realtime mode
//! (`x-use-inkbox-*: false`) is intentionally not implemented here; the default
//! Inkbox STT/TTS path needs no external model.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::header::{HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use zeroclaw_api::channel::ChannelMessage;

use super::inbound::AppState;

/// Per-process monotonic id for each live call leg. A local id (rather than the
/// Inkbox call id) keeps the reply registry self-contained: we mint it on
/// connect and stamp it into the `reply_target`.
static CONN_SEQ: AtomicU64 = AtomicU64::new(1);

/// Live call legs: `conn_id` → a sender the channel's `send()` pushes the
/// agent's reply text into. The bridge's writer turns each into Inkbox `text`
/// frames. Entries are removed when the socket closes.
static CALL_SINKS: OnceLock<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<String>>>> =
    OnceLock::new();

fn call_sinks() -> &'static Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<String>>> {
    CALL_SINKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Speak the agent's reply on a live call. Returns `false` when the call has
/// already hung up (the `conn_id` is no longer registered). Called by
/// [`InkboxChannel::send`](super::InkboxChannel) for `call:<conn_id>` targets.
pub(super) fn speak_to_call(conn_id: &str, text: &str) -> bool {
    match call_sinks().lock().get(conn_id) {
        Some(tx) => tx.send(text.to_string()).is_ok(),
        None => false,
    }
}

/// Upgrade the call-media connection, asking Inkbox to run STT + TTS, and hand
/// the socket to the bridge loop.
pub async fn ws_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    // Realtime mode: ask Inkbox to send RAW audio (no STT/TTS) and bridge it to
    // the OpenAI Realtime API. Falls back to Inkbox STT/TTS when not configured.
    if let Some(rt) = state.realtime.clone() {
        // Outbound calls carry a `context_token` written by `inkbox_place_call`;
        // it resolves the purpose/opening to inject. Inbound calls have none.
        let meta = super::realtime::load_call_meta(params.get("context_token").map(String::as_str));
        // Pre-flight the OpenAI connection so `realtime_fallback` can drop to
        // Inkbox STT/TTS when the model is unreachable, instead of a dead call.
        match super::realtime::connect_openai(&rt).await {
            Ok(openai) => {
                let tx = state.tx.clone();
                let alias = state.alias.clone();
                let client = state.inkbox.clone();
                let identity = state.identity.clone();
                let mut resp = ws
                    .on_upgrade(move |socket| {
                        super::realtime::run_realtime_bridge(
                            socket, openai, rt, meta, tx, alias, client, identity,
                        )
                    })
                    .into_response();
                let headers = resp.headers_mut();
                headers.insert(
                    HeaderName::from_static("x-use-inkbox-speech-to-text"),
                    HeaderValue::from_static("false"),
                );
                headers.insert(
                    HeaderName::from_static("x-use-inkbox-text-to-speech"),
                    HeaderValue::from_static("false"),
                );
                return resp;
            }
            Err(e) if rt.fallback => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    format!("[inkbox] realtime connect failed; falling back to Inkbox STT/TTS: {e}"),
                );
                // fall through to the STT/TTS path below
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    format!("[inkbox] realtime connect failed and fallback disabled: {e}"),
                );
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "realtime bridge unavailable",
                )
                    .into_response();
            }
        }
    }

    let mut resp = ws.on_upgrade(move |socket| bridge(socket, state)).into_response();
    // These ride back on the 101 the tunnel relays to Inkbox: run Inkbox's own
    // speech<->text so this leg is a text duplex (no external audio model).
    let headers = resp.headers_mut();
    headers.insert(
        HeaderName::from_static("x-use-inkbox-speech-to-text"),
        HeaderValue::from_static("true"),
    );
    headers.insert(
        HeaderName::from_static("x-use-inkbox-text-to-speech"),
        HeaderValue::from_static("true"),
    );
    resp
}

type WsSink = SplitSink<WebSocket, Message>;

/// Send one agent turn as a `text` delta + done pair. Returns `false` if the
/// socket write failed (caller should tear the bridge down).
async fn speak_turn(sink: &mut WsSink, turn_id: &str, text: &str) -> bool {
    let delta = json!({ "event": "text", "delta": text, "turn_id": turn_id }).to_string();
    if sink.send(Message::Text(delta.into())).await.is_err() {
        return false;
    }
    let done = json!({ "event": "text", "done": true, "turn_id": turn_id }).to_string();
    sink.send(Message::Text(done.into())).await.is_ok()
}

/// Bridge one call: greet on `start`, forward each final transcript to the
/// agent, and stream replies back as TTS until either side closes.
async fn bridge(socket: WebSocket, state: AppState) {
    let conn_id = format!("c{}", CONN_SEQ.fetch_add(1, Ordering::Relaxed));
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    call_sinks().lock().insert(conn_id.clone(), reply_tx);

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        format!("[inkbox] call-media WebSocket connected (conn={conn_id})"),
    );

    let (mut sink, mut stream) = socket.split();

    loop {
        tokio::select! {
            // Agent reply -> speak it back to the caller.
            reply = reply_rx.recv() => {
                match reply {
                    Some(text) if !text.trim().is_empty() => {
                        if !speak_turn(&mut sink, "agent", &text).await {
                            break;
                        }
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            // Inbound Inkbox frame.
            inbound = stream.next() => {
                let raw = match inbound {
                    Some(Ok(Message::Text(raw))) => raw,
                    Some(Ok(_)) => continue, // ignore ping/pong/binary/close-data
                    _ => break,              // stream end or socket error
                };
                let Ok(frame) = serde_json::from_str::<Value>(&raw) else { continue };
                match frame.get("event").and_then(Value::as_str) {
                    // Call connected: greet so the caller hears something on pickup.
                    Some("start") => {
                        if !speak_turn(&mut sink, "greeting", "Hi there, how can I help?").await {
                            break;
                        }
                    }
                    // Final caller utterance: hand it to the agent. Its reply
                    // returns via `send()` -> `speak_to_call` -> `reply_rx`.
                    Some("transcript") if frame.get("is_final").and_then(Value::as_bool) == Some(true) => {
                        let text = frame.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        let turn_id = frame
                            .get("turn_id")
                            .and_then(Value::as_str)
                            .unwrap_or(&conn_id)
                            .to_string();
                        let mut cm = ChannelMessage::new(
                            turn_id,
                            "caller",
                            format!("call:{conn_id}"),
                            text,
                            "inkbox",
                            now_secs(),
                        );
                        cm.channel_alias = Some(state.alias.clone());
                        // Drop on backpressure rather than wedge the call leg.
                        let _ = state.tx.try_send(cm);
                    }
                    _ => {}
                }
            }
        }
    }

    call_sinks().lock().remove(&conn_id);
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        format!("[inkbox] call-media WebSocket closed (conn={conn_id})"),
    );
}

/// Seconds since the Unix epoch, for the `ChannelMessage` timestamp.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
