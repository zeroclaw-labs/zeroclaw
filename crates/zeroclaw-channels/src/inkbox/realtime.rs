//! OpenAI Realtime bridge for Inkbox calls — a faithful port of the hermes
//! plugin's `realtime.py`.
//!
//! In realtime mode the call-media WebSocket is accepted with
//! `x-use-inkbox-speech-to-text: false` / `x-use-inkbox-text-to-speech: false`,
//! so Inkbox streams **raw g711 (PCMU) audio** as `media` frames. This bridge:
//!
//! * pumps audio both ways (Inkbox `media` ↔ OpenAI
//!   `input_audio_buffer.append` / `response.output_audio.delta`),
//! * handles barge-in (`input_audio_buffer.speech_started` → `clear`),
//! * exposes the realtime function tools the model can call mid-call:
//!   `agent_consult` (pause and ask the main agent for tool work),
//!   `register/edit/delete_post_call_action`, and two-step `hang_up_call`,
//! * accumulates the transcript, and on hangup dispatches either the queued
//!   post-call actions or a `[call_ended]` reflection turn to the agent.
//!
//! `agent_consult` reuses the channel's reply path: the bridge emits a
//! `ChannelMessage` tagged `consult:<id>`, the agent answers, and
//! [`InkboxChannel::send`](super::InkboxChannel) routes that answer back via
//! [`deliver_consult`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use zeroclaw_api::channel::ChannelMessage;

/// Realtime bridge configuration, resolved from `[channels.inkbox.<alias>]`.
#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    /// OpenAI API key (Bearer).
    pub api_key: String,
    /// Realtime model id (e.g. `gpt-realtime-2`).
    pub model: String,
    /// Voice (e.g. `cedar`).
    pub voice: String,
    /// Fall back to Inkbox STT/TTS if the realtime bridge can't connect.
    pub fallback: bool,
}

impl RealtimeConfig {
    /// Whether realtime is usable (enabled + a credential present).
    pub fn usable(enabled: bool, api_key: &str) -> bool {
        enabled && !api_key.trim().is_empty()
    }
}

/// Minimal call metadata available at WS-accept time. Purpose/opening context
/// (from the outbound `context_token`) is threaded in the context increment.
#[derive(Debug, Clone, Default)]
pub struct CallMeta {
    pub call_id: String,
    pub direction: String,
    pub contact_name: Option<String>,
    /// Outbound-call purpose (why we're calling), when known.
    pub purpose: Option<String>,
    /// Outbound opening line to say verbatim, when set.
    pub opening: Option<String>,
    /// Our own agent identity (resolved at call start) so the model speaks as
    /// ZeroClaw with the right contact details.
    pub agent_handle: String,
    pub agent_email: Option<String>,
    pub agent_phone: Option<String>,
}

const OPENAI_REALTIME_URL: &str = "wss://api.openai.com/v1/realtime";
const INPUT_TRANSCRIPTION_MODEL: &str = "gpt-4o-mini-transcribe";
const CONSULT_TIMEOUT_SECS: u64 = 60;
const HANGUP_CONFIRM_WINDOW_SECS: u64 = 60;

// ── consult reply registry ────────────────────────────────────────────────

static CONSULT_SEQ: AtomicU64 = AtomicU64::new(1);
static CONSULT_SINKS: OnceLock<Mutex<HashMap<String, oneshot::Sender<String>>>> = OnceLock::new();

fn consult_sinks() -> &'static Mutex<HashMap<String, oneshot::Sender<String>>> {
    CONSULT_SINKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Deliver the main agent's answer to a pending `agent_consult` so the realtime
/// model can read it back. Called by [`InkboxChannel::send`](super::InkboxChannel)
/// for `consult:<id>` reply targets. Returns `false` if the consult expired.
pub(super) fn deliver_consult(id: &str, answer: &str) -> bool {
    match consult_sinks().lock().remove(id) {
        Some(tx) => tx.send(answer.to_string()).is_ok(),
        None => false,
    }
}

/// Shared dir where `inkbox_place_call` drops single-use outbound-call context
/// keyed by `context_token`. Both crates derive the same path from the temp dir.
fn call_context_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("inkbox_call_contexts")
}

/// Resolve [`CallMeta`] for an incoming call-media WS from its `context_token`
/// query param. A token (set by `inkbox_place_call`) means an outbound call;
/// we load the queued purpose/opening and delete the file (single-use). No
/// token means an inbound call.
pub(super) fn load_call_meta(context_token: Option<&str>) -> CallMeta {
    let Some(token) = context_token.map(str::trim).filter(|t| !t.is_empty()) else {
        return CallMeta { direction: "inbound".into(), ..Default::default() };
    };
    let path = call_context_dir().join(format!("{token}.json"));
    let meta = match std::fs::read_to_string(&path) {
        Ok(s) => {
            let v: Value = serde_json::from_str(&s).unwrap_or_else(|_| json!({}));
            let pick = |k: &str| {
                v.get(k)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            CallMeta {
                call_id: String::new(),
                direction: "outbound".into(),
                contact_name: None,
                purpose: pick("purpose"),
                opening: pick("opening_message"),
                ..Default::default()
            }
        }
        // Token present but file gone: still an outbound call, just no context.
        Err(_) => CallMeta { direction: "outbound".into(), ..Default::default() },
    };
    let _ = std::fs::remove_file(&path);
    meta
}

// ── instructions / greeting / session ──────────────────────────────────────

fn build_instructions(meta: &CallMeta) -> String {
    let name = if meta.agent_handle.is_empty() {
        "ZeroClaw".to_string()
    } else {
        format!("ZeroClaw (agent handle \"{}\")", meta.agent_handle)
    };
    let mut s = format!(
        "You are {name}, an AI assistant, on a live phone call. You ARE the assistant the caller \
         is talking to — always speak in the first person. Never refer to yourself in the third \
         person, and never say you'll \"ask the main agent\", \"check with the team\", or \"the \
         backend\": you do the work yourself.\n"
    );
    let mut id_bits = Vec::new();
    if let Some(e) = meta.agent_email.as_deref().filter(|e| !e.is_empty()) {
        id_bits.push(format!("your email address is {e}"));
    }
    if let Some(p) = meta.agent_phone.as_deref().filter(|p| !p.is_empty()) {
        id_bits.push(format!("your phone number is {p}"));
    }
    if !id_bits.is_empty() {
        s.push_str(&format!("Your identity: {}.\n", id_bits.join("; ")));
    }
    let who = meta.contact_name.as_deref().unwrap_or("the caller");
    s.push_str(&format!("You are speaking with {who}.\n"));
    if meta.direction == "outbound" {
        s.push_str("You placed this call.\n");
        if let Some(p) = meta.purpose.as_deref().filter(|p| !p.is_empty()) {
            s.push_str(&format!("Reason you're calling: {p}\n"));
        }
    }
    s.push_str(
        "\nSpeak naturally and concisely — one short turn at a time. This is spoken audio: no \
         markdown, no long monologues.\n\n\
         When the caller needs current information, something from your memory, or an action — \
         look up or send an email or text, check or save a contact, take a note — use the \
         `agent_consult` tool. It briefly pauses the call, does the work with your full toolset, \
         and returns the result for you to read back. This is YOU doing the work; never describe \
         it as asking someone else. Don't use it for greetings or small talk.\n\
         To queue follow-up work for after the call (send an email/text, save a note, update a \
         contact), use `register_post_call_action` — tell the caller it's queued, not already \
         done; `edit_post_call_action` / `delete_post_call_action` adjust the queue.\n\
         To end the call use `hang_up_call`: it is two-step — the first call prompts you to say a \
         brief goodbye, then call it again to actually hang up. Only when the caller is done.",
    );
    s
}

fn build_greeting(meta: &CallMeta) -> String {
    if let Some(open) = meta.opening.as_deref().filter(|o| !o.is_empty()) {
        return format!("Open the call by saying this naturally as the very first thing, with no greeting before it:\n{open}");
    }
    if meta.direction == "outbound" {
        return meta
            .purpose
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| format!("Open the call: greet the person and immediately explain why you're calling: {p} One short sentence, then wait."))
            .unwrap_or_else(|| "Open the call: greet the person and concisely explain why you're calling. One short sentence, then wait.".to_string());
    }
    let who = meta
        .contact_name
        .as_deref()
        .map(|n| n.split_whitespace().next().unwrap_or(n).to_string())
        .unwrap_or_else(|| "there".to_string());
    format!("Greet the caller now: say something like \"Hi {who}, this is ZeroClaw — how can I help?\" One short sentence, then wait.")
}

/// The realtime function tools exposed to the model (ported from hermes).
fn realtime_tools() -> Value {
    json!([
        {
            "type": "function",
            "name": "agent_consult",
            "description": "Do work that needs your tools — look up or send an email/text, check or save a contact, recall something from memory, hit an API, or compute. Briefly pauses the call, does the work with your full toolset, and returns the answer for you to read back. This is YOU doing the work, not a handoff to anyone. Use whenever the caller needs current data, memory, or an action; never for greetings or small talk.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to ask the main agent in plain English, with enough context to act standalone." }
                },
                "required": ["query"]
            }
        },
        {
            "type": "function",
            "name": "register_post_call_action",
            "description": "Register work the main agent must do after this call ends — send an email/SMS follow-up, create a note, update a contact, etc. Tell the caller it's queued; do NOT claim it's already done.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "Plain-English task for the main agent. Include channel, recipient, and outcome." },
                    "details": { "type": "string", "description": "Optional draft text, hints, or constraints." }
                },
                "required": ["action"]
            }
        },
        {
            "type": "function",
            "name": "edit_post_call_action",
            "description": "Edit a previously registered after-call action. Use the one-based action_index from register_post_call_action when the caller changes the recipient, channel, wording, or scope.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action_index": { "type": "integer", "minimum": 1 },
                    "action": { "type": "string", "description": "Replacement task. Omit to keep the current task." },
                    "details": { "type": "string", "description": "Replacement details. Empty string clears them." }
                },
                "required": ["action_index"]
            }
        },
        {
            "type": "function",
            "name": "delete_post_call_action",
            "description": "Delete a previously registered after-call action by its one-based action_index when the caller cancels it.",
            "parameters": {
                "type": "object",
                "properties": { "action_index": { "type": "integer", "minimum": 1 } },
                "required": ["action_index"]
            }
        },
        {
            "type": "function",
            "name": "hang_up_call",
            "description": "End the live call. TWO-STEP: the first call does NOT hang up — it prompts you to say a short goodbye. After you've said goodbye, call it a second time to actually end the call. Use only when the caller asks to hang up, says goodbye, or the conversation is clearly complete.",
            "parameters": {
                "type": "object",
                "properties": { "reason": { "type": "string", "description": "Optional short reason for ending." } },
                "required": []
            }
        }
    ])
}

fn session_update(cfg: &RealtimeConfig, meta: &CallMeta) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "model": cfg.model,
            "instructions": build_instructions(meta),
            "output_modalities": ["audio"],
            "audio": {
                "input": {
                    "format": { "type": "audio/pcmu" },
                    "transcription": { "model": INPUT_TRANSCRIPTION_MODEL },
                    "turn_detection": {
                        "type": "server_vad",
                        "threshold": 0.5,
                        "prefix_padding_ms": 300,
                        "silence_duration_ms": 500,
                        "create_response": true,
                        "interrupt_response": true
                    }
                },
                "output": { "format": { "type": "audio/pcmu" }, "voice": cfg.voice }
            },
            "tools": realtime_tools(),
            "tool_choice": "auto"
        }
    })
}

// ── OpenAI write helpers (JSON frames) ──────────────────────────────────────

fn response_create_empty() -> WsMessage {
    WsMessage::Text(json!({ "type": "response.create" }).to_string().into())
}

fn response_create_instructions(instructions: &str) -> WsMessage {
    WsMessage::Text(
        json!({ "type": "response.create", "response": { "instructions": instructions } })
            .to_string()
            .into(),
    )
}

fn function_call_output(call_id: &str, output: &Value) -> WsMessage {
    WsMessage::Text(
        json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call_output",
                "call_id": call_id,
                "output": output.to_string()
            }
        })
        .to_string()
        .into(),
    )
}

/// A connected OpenAI Realtime WebSocket stream.
pub(super) type OpenAiWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connect to the OpenAI Realtime API (Bearer auth). The voice handler calls
/// this as a pre-flight so `realtime_fallback` can drop to Inkbox STT/TTS when
/// the model is unreachable, and passes the live stream into the bridge.
pub(super) async fn connect_openai(cfg: &RealtimeConfig) -> anyhow::Result<OpenAiWs> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let url = format!(
        "{OPENAI_REALTIME_URL}?model={}",
        urlencoding::encode(&cfg.model)
    );
    let mut request = url
        .into_client_request()
        .map_err(|e| anyhow::anyhow!("realtime: bad URL: {e}"))?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", cfg.api_key)
            .parse()
            .map_err(|e| anyhow::anyhow!("realtime: bad auth header: {e}"))?,
    );
    let (ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| anyhow::anyhow!("realtime: OpenAI connect failed: {e}"))?;
    Ok(ws)
}

/// One in-flight function call accumulating streamed arguments.
struct PendingCall {
    call_id: String,
    name: String,
    args: String,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Run the realtime bridge between the (already-upgraded) Inkbox call-media
/// WebSocket and the OpenAI Realtime API. Returns when either side closes.
#[allow(clippy::too_many_arguments)]
pub async fn run_realtime_bridge(
    inkbox_ws: WebSocket,
    openai: OpenAiWs,
    cfg: RealtimeConfig,
    meta: CallMeta,
    tx: mpsc::Sender<ChannelMessage>,
    alias: String,
    client: Arc<inkbox::Inkbox>,
    identity: String,
) {
    // Resolve our own identity so the model introduces itself as ZeroClaw with
    // the right email/phone (blocking SDK call on the blocking pool).
    let mut meta = meta;
    {
        let client = client.clone();
        let handle = identity.clone();
        if let Ok((h, email, phone)) = tokio::task::spawn_blocking(move || match client
            .get_identity(&handle)
        {
            Ok(id) => (
                id.agent_handle(),
                id.email_address(),
                id.phone_number().map(|p| p.number),
            ),
            Err(_) => (handle, None, None),
        })
        .await
        {
            meta.agent_handle = h;
            meta.agent_email = email;
            meta.agent_phone = phone;
        }
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "[inkbox] realtime bridge connected to OpenAI",
    );

    let (mut oai_tx, mut oai_rx) = openai.split();
    let (mut ink_tx, mut ink_rx) = inkbox_ws.split();

    // Single OpenAI writer: every OpenAI-bound frame (audio append, greeting,
    // tool outputs from async consult tasks) flows through this channel so the
    // sink has exactly one writer.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<WsMessage>();
    if out_tx.send(WsMessage::Text(session_update(&cfg, &meta).to_string().into())).is_err() {
        return;
    }

    let mut stream_id: Option<String> = None;
    let mut greeting_sent = false;
    let mut transcript: Vec<(String, String)> = Vec::new();
    let mut post_call_actions: Vec<(String, String)> = Vec::new();
    let mut pending: HashMap<String, PendingCall> = HashMap::new();
    let mut hangup_armed_at: Option<Instant> = None;
    let mut closing = false;

    let greet_once = |out_tx: &mpsc::UnboundedSender<WsMessage>, sent: &mut bool| {
        if !*sent {
            *sent = true;
            let _ = out_tx.send(response_create_instructions(&build_greeting(&meta)));
        }
    };

    loop {
        tokio::select! {
            // ── drain OpenAI-bound frames ──
            Some(frame) = out_rx.recv() => {
                if oai_tx.send(frame).await.is_err() { break; }
            }
            // ── Inkbox → OpenAI ──
            ink = ink_rx.next() => {
                let raw = match ink {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(_)) => continue,
                    _ => break,
                };
                let Ok(frame) = serde_json::from_str::<Value>(&raw) else { continue };
                match frame.get("event").and_then(Value::as_str) {
                    Some("start") => {
                        if let Some(sid) = frame.get("stream_id").and_then(Value::as_str) {
                            stream_id = Some(sid.to_string());
                        }
                        greet_once(&out_tx, &mut greeting_sent);
                    }
                    Some("media") => {
                        greet_once(&out_tx, &mut greeting_sent);
                        if let Some(payload) = frame.pointer("/media/payload").and_then(Value::as_str) {
                            let _ = out_tx.send(WsMessage::Text(
                                json!({ "type": "input_audio_buffer.append", "audio": payload })
                                    .to_string()
                                    .into(),
                            ));
                        }
                    }
                    Some("stop") | Some("closed") | Some("hangup") => break,
                    _ => {}
                }
            }
            // ── OpenAI → Inkbox / tool calls ──
            oai = oai_rx.next() => {
                let raw = match oai {
                    Some(Ok(WsMessage::Text(t))) => t,
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => break,
                };
                let Ok(ev) = serde_json::from_str::<Value>(&raw) else { continue };
                match ev.get("type").and_then(Value::as_str) {
                    Some("response.output_audio.delta") | Some("response.audio.delta") => {
                        if let Some(delta) = ev.get("delta").and_then(Value::as_str) {
                            let mut media = json!({ "event": "media", "media": { "payload": delta, "track": "outbound" } });
                            if let Some(sid) = &stream_id { media["stream_id"] = json!(sid); }
                            if ink_tx.send(Message::Text(media.to_string().into())).await.is_err() { break; }
                        }
                    }
                    Some("response.output_audio.done") | Some("response.audio.done") => {
                        let mut done = json!({ "event": "audio_done" });
                        if let Some(sid) = &stream_id { done["stream_id"] = json!(sid); }
                        let _ = ink_tx.send(Message::Text(done.to_string().into())).await;
                    }
                    Some("input_audio_buffer.speech_started") => {
                        let _ = ink_tx.send(Message::Text(json!({ "event": "clear" }).to_string().into())).await;
                    }
                    Some("response.audio_transcript.done")
                    | Some("response.output_audio_transcript.done") => {
                        if let Some(t) = ev.get("transcript").and_then(Value::as_str) {
                            transcript.push(("agent".into(), t.to_string()));
                        }
                    }
                    Some("conversation.item.input_audio_transcription.completed") => {
                        if let Some(t) = ev.get("transcript").and_then(Value::as_str) {
                            transcript.push(("caller".into(), t.to_string()));
                        }
                    }
                    // function-call streaming
                    Some("response.output_item.added") => {
                        if let Some(item) = ev.get("item") {
                            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                                if let (Some(id), Some(call_id), Some(name)) = (
                                    item.get("id").and_then(Value::as_str),
                                    item.get("call_id").and_then(Value::as_str),
                                    item.get("name").and_then(Value::as_str),
                                ) {
                                    pending.insert(id.to_string(), PendingCall {
                                        call_id: call_id.to_string(),
                                        name: name.to_string(),
                                        args: String::new(),
                                    });
                                }
                            }
                        }
                    }
                    Some("response.function_call_arguments.delta") => {
                        if let (Some(id), Some(delta)) = (
                            ev.get("item_id").and_then(Value::as_str),
                            ev.get("delta").and_then(Value::as_str),
                        ) {
                            if let Some(p) = pending.get_mut(id) { p.args.push_str(delta); }
                        }
                    }
                    Some("response.function_call_arguments.done") => {
                        let id = ev.get("item_id").and_then(Value::as_str).unwrap_or("");
                        let pc = pending.remove(id).or_else(|| {
                            // fallback: some streams only carry the done frame
                            Some(PendingCall {
                                call_id: ev.get("call_id").and_then(Value::as_str)?.to_string(),
                                name: ev.get("name").and_then(Value::as_str)?.to_string(),
                                args: ev.get("arguments").and_then(Value::as_str).unwrap_or("{}").to_string(),
                            })
                        });
                        let Some(pc) = pc else { continue };
                        let args: Value = serde_json::from_str(&pc.args).unwrap_or_else(|_| json!({}));
                        let mut send_output = true;
                        let output: Value = match pc.name.as_str() {
                            "agent_consult" => {
                                send_output = false; // the spawned task posts the output
                                dispatch_consult(&args, &pc.call_id, &tx, &alias, &meta, &out_tx);
                                json!({})
                            }
                            "register_post_call_action" => {
                                let action = args.get("action").and_then(Value::as_str).unwrap_or("").trim().to_string();
                                if action.is_empty() {
                                    json!({ "error": "action is required" })
                                } else {
                                    let details = args.get("details").and_then(Value::as_str).unwrap_or("").trim().to_string();
                                    post_call_actions.push((action, details));
                                    json!({ "status": "queued", "action_index": post_call_actions.len(), "action_count": post_call_actions.len(), "message": "Queued for after the call." })
                                }
                            }
                            "edit_post_call_action" => {
                                let idx = args.get("action_index").and_then(Value::as_i64).unwrap_or(0);
                                if idx < 1 || idx as usize > post_call_actions.len() {
                                    json!({ "error": "invalid action_index" })
                                } else {
                                    let slot = &mut post_call_actions[(idx - 1) as usize];
                                    if let Some(a) = args.get("action").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) { slot.0 = a.trim().to_string(); }
                                    if let Some(d) = args.get("details").and_then(Value::as_str) { slot.1 = d.trim().to_string(); }
                                    json!({ "status": "updated", "action_index": idx, "action_count": post_call_actions.len() })
                                }
                            }
                            "delete_post_call_action" => {
                                let idx = args.get("action_index").and_then(Value::as_i64).unwrap_or(0);
                                if idx < 1 || idx as usize > post_call_actions.len() {
                                    json!({ "error": "invalid action_index" })
                                } else {
                                    post_call_actions.remove((idx - 1) as usize);
                                    json!({ "status": "deleted", "action_count": post_call_actions.len() })
                                }
                            }
                            "hang_up_call" => {
                                let recent = hangup_armed_at
                                    .map(|t| t.elapsed() < Duration::from_secs(HANGUP_CONFIRM_WINDOW_SECS))
                                    .unwrap_or(false);
                                if recent {
                                    // Second call within the window → actually end.
                                    let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
                                    let mut hangup = json!({ "event": "hangup", "reason": reason });
                                    if let Some(sid) = &stream_id { hangup["stream_id"] = json!(sid); }
                                    let _ = ink_tx.send(Message::Text(hangup.to_string().into())).await;
                                    closing = true;
                                    json!({ "status": "hangup_requested", "message": "The call is ending now." })
                                } else {
                                    hangup_armed_at = Some(Instant::now());
                                    json!({ "status": "confirm_goodbye", "message": "Don't hang up yet. Say a brief, natural goodbye, then call hang_up_call again." })
                                }
                            }
                            other => json!({ "error": format!("unknown tool {other}") }),
                        };
                        if send_output {
                            let _ = out_tx.send(function_call_output(&pc.call_id, &output));
                            if !closing {
                                let _ = out_tx.send(response_create_empty());
                            }
                        }
                        if closing { break; }
                    }
                    _ => {}
                }
            }
        }
    }

    // Post-call: dispatch queued actions, else a reflection turn.
    dispatch_post_call(&tx, &alias, &meta, &transcript, &post_call_actions);

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        format!("[inkbox] realtime call ended ({} transcript turns, {} post-call actions)", transcript.len(), post_call_actions.len()),
    );
}

/// Dispatch an `agent_consult`: emit the query as a `ChannelMessage` the agent
/// answers, send the model an interim "one moment", and post the answer back as
/// the tool output once it arrives (with a timeout).
fn dispatch_consult(
    args: &Value,
    call_id: &str,
    tx: &mpsc::Sender<ChannelMessage>,
    alias: &str,
    meta: &CallMeta,
    out_tx: &mpsc::UnboundedSender<WsMessage>,
) {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if query.is_empty() {
        let _ = out_tx.send(function_call_output(call_id, &json!({ "error": "query is required" })));
        let _ = out_tx.send(response_create_empty());
        return;
    }
    let id = format!("rc{}", CONSULT_SEQ.fetch_add(1, Ordering::Relaxed));
    let (otx, orx) = oneshot::channel::<String>();
    consult_sinks().lock().insert(id.clone(), otx);

    // The agent answers this like any inbound message; its reply routes back to
    // us via `deliver_consult` (reply_target `consult:<id>`).
    let mut cm = ChannelMessage::new(
        format!("consult:{}", meta.call_id),
        meta.contact_name.clone().unwrap_or_else(|| "caller".into()),
        format!("consult:{id}"),
        format!("[in-call consult] {query}"),
        "inkbox",
        now_secs(),
    );
    cm.channel_alias = Some(alias.to_string());
    let _ = tx.try_send(cm);

    // Interim acknowledgement so the caller isn't left in silence.
    let _ = out_tx.send(response_create_instructions(
        "Say only 'One moment.' Do not mention waiting for a lookup.",
    ));

    let out_tx = out_tx.clone();
    let call_id = call_id.to_string();
    tokio::spawn(async move {
        let answer = match tokio::time::timeout(Duration::from_secs(CONSULT_TIMEOUT_SECS), orx).await {
            Ok(Ok(ans)) => ans,
            _ => {
                consult_sinks().lock().remove(&id);
                "I couldn't reach the assistant just now — let's continue.".to_string()
            }
        };
        let output = json!({ "status": "ok", "answer": answer, "instructions": "Read this answer back to the caller, naturally." });
        let _ = out_tx.send(function_call_output(&call_id, &output));
        let _ = out_tx.send(response_create_empty());
    });
}

/// On call end, hand the agent either the queued post-call actions or a
/// `[call_ended]` reflection, with the transcript, via a `ChannelMessage`.
fn dispatch_post_call(
    tx: &mpsc::Sender<ChannelMessage>,
    alias: &str,
    meta: &CallMeta,
    transcript: &[(String, String)],
    post_call_actions: &[(String, String)],
) {
    let transcript_block: String = transcript
        .iter()
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|(role, text)| format!("  - {role}: {text}"))
        .collect::<Vec<_>>()
        .join("\n");

    let body = if post_call_actions.is_empty() {
        let mut b = format!(
            "[inkbox:voice_call call_id={}]\n[call_ended] The realtime voice call ended. \
             Reflect and decide if any follow-up is needed:\n\
             - if you committed to anything during the call (send an email/SMS, save a note, \
             update a contact), do it now via tool calls.\n\
             - if there's nothing to do, reply with exactly [SILENT] and nothing else.\n\
             Any plain-text reply here is suppressed; side effects must come from tool calls.",
            meta.call_id
        );
        if !transcript_block.is_empty() {
            b.push_str(&format!("\n\nRecent transcript:\n{transcript_block}"));
        }
        b
    } else {
        let actions: String = post_call_actions
            .iter()
            .enumerate()
            .map(|(i, (a, d))| {
                if d.is_empty() {
                    format!("  {}. {a}", i + 1)
                } else {
                    format!("  {}. {a} (details: {d})", i + 1)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut b = format!(
            "[inkbox:voice_post_call_actions call_id={}]\nThe realtime voice call ended. Execute \
             the still-needed queued post-call actions now via tool calls (send email/SMS, save \
             a note, update a contact). Don't re-do anything already handled.\n\nQueued actions:\n{actions}",
            meta.call_id
        );
        if !transcript_block.is_empty() {
            b.push_str(&format!("\n\nFull call transcript:\n{transcript_block}"));
        }
        b
    };

    // Inbound calls keep the per-call thread; outbound joins the contact thread.
    let reply_target = if meta.direction == "outbound" {
        "noreply".to_string()
    } else {
        format!("call:{}", meta.call_id)
    };
    let mut cm = ChannelMessage::new(
        format!("call:{}:post", meta.call_id),
        meta.contact_name.clone().unwrap_or_else(|| "caller".into()),
        reply_target,
        body,
        "inkbox",
        now_secs(),
    );
    cm.channel_alias = Some(alias.to_string());
    let _ = tx.try_send(cm);
}
