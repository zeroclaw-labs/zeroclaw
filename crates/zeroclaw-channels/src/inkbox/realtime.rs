//! OpenAI Realtime bridge for Inkbox calls.
//!
//! In realtime mode the call-media WebSocket is accepted with
//! `x-use-inkbox-speech-to-text: false` / `x-use-inkbox-text-to-speech: false`,
//! so Inkbox streams **raw g711 (PCMU) audio** as `media` frames. This bridge:
//!
//! * pumps audio both ways (Inkbox `media` ↔ OpenAI
//!   `input_audio_buffer.append` / `response.output_audio.delta`),
//! * handles barge-in (`input_audio_buffer.speech_started` → `clear`),
//! * exposes the realtime function tools the model can call mid-call:
//!   `consult_agent` (pause and ask the main agent for tool work),
//!   `register/edit/delete_post_call_action`, and two-step `hang_up_call`,
//! * accumulates the transcript, and on hangup dispatches either the queued
//!   post-call actions or a `[call_ended]` reflection turn to the agent.
//!
//! `consult_agent` reuses the channel's reply path: the bridge emits a
//! `ChannelMessage` tagged `consult:<id>`, the agent answers, and
//! [`InkboxChannel::send`](super::InkboxChannel) routes that answer back via
//! [`deliver_consult`].

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use zeroclaw_api::channel::ChannelMessage;

/// Realtime bridge configuration, resolved from `[channels.inkbox.<alias>]`.
#[derive(Clone)]
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

// Hand-written so a stray `{:?}` can never print the OpenAI API key.
impl std::fmt::Debug for RealtimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeConfig")
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("fallback", &self.fallback)
            .finish()
    }
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
    /// Inkbox call id (UUID string); empty on the STT/TTS path.
    pub call_id: String,
    /// `"inbound"` or `"outbound"` — drives greeting/instruction wording.
    pub direction: String,
    /// Resolved caller display name, when a contact matched.
    pub contact_name: Option<String>,
    /// The caller's full Inkbox contact card (every populated field), rendered
    /// for the model — so the agent can act on "send me an email" / "text my
    /// other number" without asking who they are or how to reach them.
    pub contact_card: Option<String>,
    /// Outbound-call purpose (why we're calling), when known.
    pub purpose: Option<String>,
    /// Outbound opening line to say verbatim, when set.
    pub opening: Option<String>,
    /// On an outbound call, the number we dialed — used to resolve that party's
    /// contact card (outbound legs carry no `call_id`).
    pub remote_number: Option<String>,
    /// Our own agent identity (resolved at call start) so the model speaks as
    /// ZeroClaw with the right contact details.
    pub agent_handle: String,
    /// Our own email identity, when the agent has a mailbox.
    pub agent_email: Option<String>,
    /// Our own phone number, when the agent has one.
    pub agent_phone: Option<String>,
    /// Whether this agent has the shared Inkbox iMessage line enabled. Drives the
    /// shared-line guidance so the model knows it may be on a shared line and
    /// never states or promises that line's Inkbox-managed number.
    pub agent_imessage_enabled: bool,
    /// The inbound caller's phone number (from the call record), surfaced to the
    /// model so it can look up an unknown caller. Distinct from `remote_number`,
    /// which is the number we dialed on an outbound call.
    pub remote_phone_number: Option<String>,
}

const OPENAI_REALTIME_URL: &str = "wss://api.openai.com/v1/realtime";
const INPUT_TRANSCRIPTION_MODEL: &str = "gpt-4o-mini-transcribe";
const CONSULT_TIMEOUT_SECS: u64 = 300;
const HANGUP_CONFIRM_WINDOW_SECS: u64 = 60;
/// After the confirmed `hang_up_call`, hold the carrier leg open briefly so the
/// already-forwarded goodbye audio plays out before we send the hangup frame
/// and close the socket.
const HANGUP_CLOSE_DELAY_SECS: u64 = 2;

// ── consult reply registry ────────────────────────────────────────────────

static CONSULT_SEQ: AtomicU64 = AtomicU64::new(1);
static CONSULT_SINKS: OnceLock<Mutex<HashMap<String, oneshot::Sender<String>>>> = OnceLock::new();

fn consult_sinks() -> &'static Mutex<HashMap<String, oneshot::Sender<String>>> {
    CONSULT_SINKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Deliver the main agent's answer to a pending `consult_agent` so the realtime
/// model can read it back. Called by [`InkboxChannel::send`](super::InkboxChannel)
/// for `consult:<id>` reply targets. Returns `false` if the consult expired.
pub(super) fn deliver_consult(id: &str, answer: &str) -> bool {
    match consult_sinks().lock().remove(id) {
        Some(tx) => tx.send(answer.to_string()).is_ok(),
        None => false,
    }
}

/// Resolve [`CallMeta`] for an incoming call-media WS from its `context_token`
/// query param. A token (set by `inkbox_place_call`) means an outbound call; we
/// reclaim the queued purpose/opening from the in-process registry (single-use).
/// No token means an inbound call. A token with no matching entry is still an
/// outbound call, just without context (e.g. the daemon restarted mid-call).
pub(super) fn load_call_meta(context_token: Option<&str>) -> CallMeta {
    let Some(token) = context_token.map(str::trim).filter(|t| !t.is_empty()) else {
        return CallMeta {
            direction: "inbound".into(),
            ..Default::default()
        };
    };
    let ctx = zeroclaw_tools::inkbox::take_call_context(token);
    CallMeta {
        direction: "outbound".into(),
        purpose: ctx.as_ref().and_then(|c| c.purpose.clone()),
        opening: ctx.as_ref().and_then(|c| c.opening_message.clone()),
        remote_number: ctx.and_then(|c| c.remote_number),
        ..Default::default()
    }
}

// ── instructions / greeting / session ──────────────────────────────────────

/// Build the realtime model instructions for the ZeroClaw identity and
/// first-person `consult_agent` (it's the same agent, not a handoff).
fn build_instructions(meta: &CallMeta) -> String {
    let name = if meta.agent_handle.is_empty() {
        "ZeroClaw".to_string()
    } else {
        format!("ZeroClaw (agent handle \"{}\")", meta.agent_handle)
    };
    let mut lines: Vec<String> = vec![
        format!(
            "You are {name}, speaking on a live Inkbox phone call. You ARE the assistant the \
             caller is talking to — speak in the first person; never refer to yourself in the \
             third person and never say you'll ask a 'main agent' or 'the backend'."
        ),
        "Use natural, concise spoken replies. Keep most answers to one or two short sentences."
            .into(),
        "Do not mention implementation details unless the caller asks.".into(),
    ];
    if let Some(e) = meta.agent_email.as_deref().filter(|e| !e.is_empty()) {
        lines.push(format!("Your email identity: {e}."));
    }
    if let Some(p) = meta.agent_phone.as_deref().filter(|p| !p.is_empty()) {
        lines.push(format!("Your phone number: {p}."));
    }
    if meta.agent_imessage_enabled {
        lines.push(
            "You also have a shared Inkbox iMessage line — voice calls and iMessage with people \
             connected to you over iMessage. Its number is managed by Inkbox: never state or \
             promise a number for it. The current call may be running over either line; calls \
             follow the conversation's channel (iMessage contacts are called over the shared \
             line, SMS/phone contacts over your dedicated number)."
                .into(),
        );
    }
    if let Some(n) = meta
        .remote_phone_number
        .as_deref()
        .filter(|n| !n.is_empty())
    {
        lines.push(format!("Caller is calling from: {n}."));
    }
    match meta
        .contact_name
        .as_deref()
        .filter(|c| !c.is_empty() && *c != "caller")
    {
        Some(c) => {
            lines.push(
                "You already know who this is — do NOT look them up or ask for details you \
                 already have below."
                    .into(),
            );
            lines.push(format!("Caller name: {c}."));
            // The full contact card: every email/phone/address on file, so the
            // agent can act on "email me" / "text my other line" directly.
            if let Some(card) = meta
                .contact_card
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                lines.push(format!("Their full contact card:\n{card}"));
            }
        }
        None => lines.push(
            "No matching contact record is loaded — you do NOT know who this is. Greet them \
             neutrally; you may look them up by phone number if needed."
                .into(),
        ),
    }
    if meta.direction == "outbound" {
        if let Some(p) = meta.purpose.as_deref().filter(|p| !p.is_empty()) {
            lines.push(format!("This is an outbound call you placed. Purpose: {p}"));
        }
        if let Some(o) = meta.opening.as_deref().filter(|o| !o.is_empty()) {
            lines.push(format!(
                "Preferred opening message (say this naturally as your first turn): {o}"
            ));
        }
        lines.push(
            "For outbound calls, do not open with a generic offer to help. Start by explaining \
             why you are calling, then ask the next specific question."
                .into(),
        );
    }
    lines.extend([
        "Do not perform a lookup before greeting the caller. Do not say you are waiting on a \
         lookup or checking context."
            .to_string(),
        "If the caller asks for work to happen now during the live call and it needs your tools, \
         call consult_agent. This includes sending SMS/email, reading SMS/email/call history, \
         creating notes, updating contacts, or checking current data. It is YOU doing the work \
         with your own tools, not a handoff to anyone else."
            .to_string(),
        "If the caller explicitly asks for work to happen after the call, or accepts an after-call \
         deferral, call register_post_call_action. Tell the caller the action is queued for after \
         the call; do not claim it has already been completed."
            .to_string(),
        "If the caller changes or cancels previously queued after-call work, call \
         edit_post_call_action or delete_post_call_action using the action index returned when \
         the work was queued."
            .to_string(),
        "If consult_agent completes or queues work that matches a previously registered after-call \
         action, call delete_post_call_action for that action so it is not executed twice after \
         hangup."
            .to_string(),
        "If the caller asks to hang up, says goodbye, or the conversation is clearly complete, \
         say a brief, natural goodbye and call hang_up_call. The call ends automatically once \
         your goodbye finishes — you do not need to call it again."
            .to_string(),
        "Do not call consult_agent for greetings, caller identity at call start, or generic chat."
            .to_string(),
    ]);
    lines.join("\n")
}

/// Build the realtime greeting for the ZeroClaw agent.
fn build_greeting(meta: &CallMeta) -> String {
    let first_name = meta
        .contact_name
        .as_deref()
        .filter(|c| !c.is_empty() && *c != "caller")
        .map(|n| n.split_whitespace().next().unwrap_or(n).to_string());

    if meta.direction == "outbound" {
        if let Some(o) = meta.opening.as_deref().filter(|o| !o.is_empty()) {
            return format!(
                "Open the call by saying this naturally as the very first thing, with no greeting before it:\n{o}"
            );
        }
        if let Some(p) = meta.purpose.as_deref().filter(|p| !p.is_empty()) {
            return format!(
                "Open the call by greeting the person and immediately explaining why you are calling: {p}"
            );
        }
        return "Open the call by greeting the person and explaining why you are calling. Be specific and concise.".to_string();
    }

    let who = first_name.unwrap_or_else(|| "there".to_string());
    format!(
        "Greet the caller now as the very first thing you say. Say something like 'Hi {who}, \
         this is ZeroClaw — how can I help?' Keep it to one short sentence and then wait for \
         them to respond."
    )
}

/// The realtime function tools exposed to the model.
fn realtime_tools() -> Value {
    json!([
        {
            "type": "function",
            "name": "consult_agent",
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
            "description": "End the live call. Call this once when the caller asks to hang up, says goodbye, or the conversation is clearly complete: say a brief, natural goodbye and the call ends automatically once it plays. You do not need to call it twice.",
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
        .map_err(|e| anyhow::Error::msg(format!("realtime: bad URL: {e}")))?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", cfg.api_key)
            .parse()
            .map_err(|e| anyhow::Error::msg(format!("realtime: bad auth header: {e}")))?,
    );
    let (ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| anyhow::Error::msg(format!("realtime: OpenAI connect failed: {e}")))?;
    Ok(ws)
}

/// One in-flight function call accumulating streamed arguments.
struct PendingCall {
    call_id: String,
    name: String,
    args: String,
}

/// Best display name for a contact: preferred name, else the assembled
/// given/middle/family parts, else the company. Used for the short references
/// (greeting line, post-call "Caller:" label).
fn contact_display_name(c: &inkbox::contacts::types::Contact) -> Option<String> {
    if let Some(p) = c.preferred_name.clone().filter(|s| !s.trim().is_empty()) {
        return Some(p);
    }
    let parts: Vec<&str> = [
        c.given_name.as_deref(),
        c.middle_name.as_deref(),
        c.family_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.trim().is_empty())
    .collect();
    if !parts.is_empty() {
        return Some(parts.join(" "));
    }
    c.company_name.clone().filter(|s| !s.trim().is_empty())
}

/// Render the caller's entire Inkbox contact card as a readable block — every
/// populated field — so the agent has the full picture (all emails, phones,
/// addresses, notes, custom fields) and can reach them however they ask.
fn render_contact_card(c: &inkbox::contacts::types::Contact) -> String {
    // Label + "(tag, tag)" suffix for the multi-value sections.
    let tagged = |value: &str, label: &Option<String>, primary: bool| -> String {
        let mut tags: Vec<String> = Vec::new();
        if let Some(l) = label.as_deref().filter(|s| !s.trim().is_empty()) {
            tags.push(l.to_string());
        }
        if primary {
            tags.push("primary".into());
        }
        if tags.is_empty() {
            value.to_string()
        } else {
            format!("{value} ({})", tags.join(", "))
        }
    };

    let mut lines: Vec<String> = Vec::new();
    let full_name: Vec<&str> = [
        c.name_prefix.as_deref(),
        c.given_name.as_deref(),
        c.middle_name.as_deref(),
        c.family_name.as_deref(),
        c.name_suffix.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.trim().is_empty())
    .collect();
    if !full_name.is_empty() {
        lines.push(format!("- Name: {}", full_name.join(" ")));
    }
    if let Some(p) = c.preferred_name.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- Goes by: {p}"));
    }
    match (
        c.company_name.as_deref().filter(|s| !s.trim().is_empty()),
        c.job_title.as_deref().filter(|s| !s.trim().is_empty()),
    ) {
        (Some(co), Some(t)) => lines.push(format!("- Company: {co} ({t})")),
        (Some(co), None) => lines.push(format!("- Company: {co}")),
        (None, Some(t)) => lines.push(format!("- Title: {t}")),
        _ => {}
    }
    if !c.emails.is_empty() {
        let v: Vec<String> = c
            .emails
            .iter()
            .map(|e| tagged(&e.value, &e.label, e.is_primary))
            .collect();
        lines.push(format!("- Emails: {}", v.join("; ")));
    }
    if !c.phones.is_empty() {
        let v: Vec<String> = c
            .phones
            .iter()
            .map(|p| tagged(&p.value, &p.label, p.is_primary))
            .collect();
        lines.push(format!("- Phones: {}", v.join("; ")));
    }
    if !c.websites.is_empty() {
        let v: Vec<String> = c
            .websites
            .iter()
            .map(|w| tagged(&w.value, &w.label, false))
            .collect();
        lines.push(format!("- Websites: {}", v.join("; ")));
    }
    if let Some(b) = c.birthday.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- Birthday: {b}"));
    }
    if !c.dates.is_empty() {
        let v: Vec<String> = c
            .dates
            .iter()
            .map(|d| tagged(&d.value, &d.label, false))
            .collect();
        lines.push(format!("- Dates: {}", v.join("; ")));
    }
    for a in &c.addresses {
        // Join only the populated address parts, in postal order.
        let parts: Vec<&str> = [
            a.street.as_deref(),
            a.city.as_deref(),
            a.region.as_deref(),
            a.postal_code.as_deref(),
            a.country.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect();
        if !parts.is_empty() {
            let label = a.label.as_deref().filter(|s| !s.trim().is_empty());
            match label {
                Some(l) => lines.push(format!("- Address ({l}): {}", parts.join(", "))),
                None => lines.push(format!("- Address: {}", parts.join(", "))),
            }
        }
    }
    for f in &c.custom_fields {
        lines.push(format!("- {}: {}", f.label, f.value));
    }
    if let Some(n) = c.notes.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- Notes: {n}"));
    }
    lines.join("\n")
}

/// Everything the realtime bridge needs besides the two live sockets and the
/// inbound-message sender: call config, seed metadata, and the SDK client +
/// identity used to resolve the caller during setup.
pub(super) struct BridgeSetup {
    pub cfg: RealtimeConfig,
    pub meta: CallMeta,
    pub alias: String,
    pub client: Arc<inkbox::Inkbox>,
    pub identity: String,
    pub call_id: String,
}

/// Run the realtime bridge between the (already-upgraded) Inkbox call-media
/// WebSocket and the OpenAI Realtime API. Returns when either side closes.
pub(super) async fn run_realtime_bridge(
    inkbox_ws: WebSocket,
    openai: OpenAiWs,
    tx: mpsc::Sender<ChannelMessage>,
    setup: BridgeSetup,
) {
    let BridgeSetup {
        cfg,
        meta,
        alias,
        client,
        identity,
        call_id,
    } = setup;
    // Resolve (a) our own identity so the model speaks as ZeroClaw, and (b) the
    // CALLER's full contact card from the call record so the agent can act on
    // requests like "send me an email" / "text my other line" without asking
    // who they are or how to reach them. Blocking SDK calls on the blocking pool.
    let mut meta = meta;
    {
        let client = client.clone();
        let handle = identity.clone();
        let call_id = call_id.clone();
        // Outbound calls carry no call_id; resolve the party from the number we
        // dialed (stashed in the call context by inkbox_place_call) instead.
        let dialed = meta.remote_number.clone();
        let resolved = tokio::task::spawn_blocking(move || {
            let ident = client.get_identity(&handle).ok();
            let (agent_handle, agent_email, agent_phone, _phone_id) = match &ident {
                Some(i) => (
                    i.agent_handle(),
                    i.email_address(),
                    i.phone_number().map(|p| p.number),
                    i.phone_number().map(|p| p.id.to_string()),
                ),
                None => (handle.clone(), None, None, None),
            };
            // Whether this agent is reachable over the shared iMessage line, so
            // the model gets the shared-line guidance (never state its number).
            let imessage_enabled = ident.as_ref().map(|i| i.imessage_enabled()).unwrap_or(false);
            let mut caller_name = None;
            let mut caller_card = None;
            let mut direction = String::new();
            // The remote party's number: from the call record on inbound (which
            // also gives us the direction), or the dialed number on outbound.
            // Calls are identity-scoped now — look the record up by call_id
            // alone (works for shared-iMessage-line calls with no dedicated
            // number too).
            let remote = if !call_id.is_empty() {
                match client.calls().get(&call_id) {
                    Ok(call) => {
                        direction = call.direction;
                        call.remote_phone_number
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                            format!("[inkbox] could not load call record {call_id}: {e}"),
                        );
                        String::new()
                    }
                }
            } else {
                dialed.unwrap_or_default()
            };
            // Inbound calls carry a call_id; surface that caller's number to the
            // model so it can look up an unknown caller (common on the shared
            // iMessage line). Outbound legs already carry the dialed number.
            let inbound_caller = if !call_id.is_empty() && !remote.is_empty() {
                Some(remote.clone())
            } else {
                None
            };
            if !remote.is_empty() {
                // Surface SDK errors: a failed lookup here is the difference
                // between "unknown caller" and "we couldn't reach Inkbox".
                match client.contacts().lookup(None, None, None, Some(&remote), None) {
                    Ok(found) => {
                        if let Some(c) = found.first() {
                            caller_name = contact_display_name(c);
                            // Reverse-lookup returns a summary (no emails/phones);
                            // fetch the full card by id, summary as fallback.
                            match client.contacts().get(&c.id.to_string()) {
                                Ok(full) => caller_card = Some(render_contact_card(&full)),
                                Err(e) => {
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                                        format!(
                                            "[inkbox] caller contact detail fetch failed for {}: {e}",
                                            c.id
                                        ),
                                    );
                                    caller_card = Some(render_contact_card(c));
                                }
                            }
                        }
                    }
                    Err(e) => ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        format!(
                            "[inkbox] caller contact lookup failed for {}: {e}",
                            super::delivery_failure::mask_target(&remote)
                        ),
                    ),
                }
            }
            (
                agent_handle,
                agent_email,
                agent_phone,
                caller_name,
                caller_card,
                direction,
                imessage_enabled,
                inbound_caller,
            )
        })
        .await;
        match resolved {
            Ok((ah, ae, ap, cn, card, dir, imsg, caller)) => {
                meta.agent_handle = ah;
                meta.agent_email = ae;
                meta.agent_phone = ap;
                meta.agent_imessage_enabled = imsg;
                if caller.is_some() {
                    meta.remote_phone_number = caller;
                }
                if cn.is_some() {
                    meta.contact_name = cn;
                }
                if card.is_some() {
                    meta.contact_card = card;
                }
                if !dir.is_empty() {
                    meta.direction = dir;
                }
            }
            // The blocking task panicked — the model loses its identity + the
            // caller's card; don't let that vanish silently.
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                format!("[inkbox] call-setup resolution task failed: {e}"),
            ),
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
    if out_tx
        .send(WsMessage::Text(
            session_update(&cfg, &meta).to_string().into(),
        ))
        .is_err()
    {
        // The writer is already gone before we configured the session — the
        // call can't proceed; log so it isn't an invisible dead call.
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "[inkbox] realtime bridge aborted before session.update could be sent",
        );
        return;
    }

    let mut stream_id: Option<String> = None;
    let mut greeting_sent = false;
    let mut transcript: Vec<(String, String)> = Vec::new();
    let mut post_call_actions: Vec<(String, String)> = Vec::new();
    let mut pending: HashMap<String, PendingCall> = HashMap::new();
    // Dedupe registry for in-call SMS/text consults (shared with the async
    // completion tasks each consult spawns).
    let consult_dedupe = Arc::new(Mutex::new(ConsultDedupe::default()));
    let mut hangup_armed_at: Option<Instant> = None;
    let mut closing = false;
    // Reason captured on the confirmed hang_up_call, sent with the hangup frame.
    let mut hangup_reason = String::new();

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
                        // Armed → this finished turn is the goodbye; end the call now
                        // (don't wait for a second hang_up_call). Let it play, then stop.
                        if hangup_armed_at.is_some() {
                            tokio::time::sleep(Duration::from_secs(HANGUP_CLOSE_DELAY_SECS)).await;
                            let mut stop = json!({ "event": "stop" });
                            if let Some(sid) = &stream_id {
                                stop["stream_id"] = json!(sid);
                            }
                            if !hangup_reason.is_empty() {
                                stop["reason"] = json!(hangup_reason);
                            }
                            let _ = ink_tx.send(Message::Text(stop.to_string().into())).await;
                            let _ = ink_tx.close().await;
                            break;
                        }
                    }
                    Some("input_audio_buffer.speech_started") => {
                        let _ = ink_tx.send(Message::Text(json!({ "event": "clear" }).to_string().into())).await;
                    }
                    Some("response.audio_transcript.done")
                    | Some("response.output_audio_transcript.done") => {
                        if let Some(t) = ev
                            .get("transcript")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            transcript.push(("agent".into(), t.to_string()));
                            // Realtime runs raw-audio, so Inkbox does no STT and would
                            // record nothing. Mirror each finalized turn back as a
                            // `transcript` frame so it lands in the Inkbox call record
                            // (party: local=agent, remote=caller).
                            let f = json!({ "event": "transcript", "party": "local", "text": t, "is_final": true });
                            let _ = ink_tx.send(Message::Text(f.to_string().into())).await;
                        }
                    }
                    Some("conversation.item.input_audio_transcription.completed") => {
                        if let Some(t) = ev
                            .get("transcript")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            transcript.push(("caller".into(), t.to_string()));
                            let f = json!({ "event": "transcript", "party": "remote", "text": t, "is_final": true });
                            let _ = ink_tx.send(Message::Text(f.to_string().into())).await;
                        }
                    }
                    // function-call streaming
                    Some("response.output_item.added") => {
                        if let Some(item) = ev.get("item")
                            && item.get("type").and_then(Value::as_str) == Some("function_call")
                            && let (Some(id), Some(call_id), Some(name)) = (
                                item.get("id").and_then(Value::as_str),
                                item.get("call_id").and_then(Value::as_str),
                                item.get("name").and_then(Value::as_str),
                            )
                        {
                            pending.insert(id.to_string(), PendingCall {
                                call_id: call_id.to_string(),
                                name: name.to_string(),
                                args: String::new(),
                            });
                        }
                    }
                    Some("response.function_call_arguments.delta") => {
                        if let (Some(id), Some(delta)) = (
                            ev.get("item_id").and_then(Value::as_str),
                            ev.get("delta").and_then(Value::as_str),
                        ) && let Some(p) = pending.get_mut(id) {
                            p.args.push_str(delta);
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
                            "consult_agent" => {
                                send_output = false; // the spawned task posts the output
                                dispatch_consult(
                                    &args,
                                    &pc.call_id,
                                    &tx,
                                    &alias,
                                    &meta,
                                    &out_tx,
                                    &consult_dedupe,
                                );
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
                                    // Second call within the window → end the call. We
                                    // submit this tool result first, then (below, after
                                    // the goodbye lands) send the hangup frame and close.
                                    hangup_reason = args
                                        .get("reason")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string();
                                    closing = true;
                                    json!({ "status": "hangup_requested", "message": "The call is ending now." })
                                } else {
                                    hangup_armed_at = Some(Instant::now());
                                    json!({ "status": "confirm_goodbye", "message": "Say a brief, natural goodbye now; the call ends automatically once it plays." })
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
                        if closing {
                            // Let the spoken goodbye land, then end the call. Inkbox
                            // tears down the carrier leg on a `stop` event from the
                            // client (it sets hangup_reason=local and fires
                            // CLIENT_HANGUP); a `hangup` event is NOT handled server-
                            // side, so that's why earlier attempts never hung up. Send
                            // `stop`, then close the socket as a backstop.
                            tokio::time::sleep(Duration::from_secs(HANGUP_CLOSE_DELAY_SECS)).await;
                            let mut stop = json!({ "event": "stop" });
                            if let Some(sid) = &stream_id {
                                stop["stream_id"] = json!(sid);
                            }
                            if !hangup_reason.is_empty() {
                                stop["reason"] = json!(hangup_reason);
                            }
                            let _ = ink_tx.send(Message::Text(stop.to_string().into())).await;
                            let _ = ink_tx.close().await;
                            break;
                        }
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
        format!(
            "[inkbox] realtime call ended ({} transcript turns, {} post-call actions)",
            transcript.len(),
            post_call_actions.len()
        ),
    );
}

/// Dispatch an `consult_agent`: emit the query as a `ChannelMessage` the agent
/// answers, send the model an interim "one moment", and post the answer back as
/// the tool output once it arrives (with a timeout).
/// Per-call dedupe state for `consult_agent`. A realtime model can fire the
/// same "text +1555… saying X" consult twice; this lets us short-circuit an
/// in-flight or already-completed duplicate so we never double-send. Mirrors the
/// hermes plugin's `pending_consult_keys` + `consult_results`.
#[derive(Default)]
struct ConsultDedupe {
    /// dedupe key -> the tool call_id currently handling it.
    pending: HashMap<String, String>,
    /// (dedupe key, result text) for consults completed this call.
    results: Vec<(String, String)>,
}

/// Lowercase, strip to `[a-z0-9+]`, collapse whitespace — mirrors hermes'
/// `_normalize_consult_text`.
fn normalize_consult_text(value: &str) -> String {
    static NON_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9+]+").unwrap());
    static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    let lowered = value.to_lowercase();
    let spaced = NON_ALNUM.replace_all(&lowered, " ");
    WS.replace_all(spaced.trim(), " ").into_owned()
}

/// The first 8–280 char quoted span in `value`, normalized — mirrors hermes'
/// `_quoted_consult_text`.
fn quoted_consult_text(value: &str) -> Option<String> {
    static QUOTED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("[\"\u{201c}]([^\"\u{201d}]{8,280})[\"\u{201d}]").unwrap());
    let cap = QUOTED.captures(value)?;
    let normalized = normalize_consult_text(cap.get(1)?.as_str());
    (!normalized.is_empty()).then_some(normalized)
}

/// Dedupe key for an SMS/text consult: `sms:<phone>:<quoted-or-generic>`, or
/// `None` when the request isn't a phone+text send. Mirrors hermes'
/// `_realtime_consult_dedupe_key`.
fn consult_dedupe_key(request: &str) -> Option<String> {
    static PHONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\+\d{8,15}").unwrap());
    static IS_SMS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b(sms|text|message)\b").unwrap());
    let normalized = normalize_consult_text(request);
    let phone = PHONE.find(&normalized)?;
    if !IS_SMS.is_match(&normalized) {
        return None;
    }
    let quoted = quoted_consult_text(request).unwrap_or_else(|| "generic".to_string());
    Some(format!("sms:{}:{}", phone.as_str(), quoted))
}

/// Whether the request explicitly asks for another/repeat message, which
/// bypasses dedupe. Mirrors hermes' `_realtime_consult_allows_repeat`.
fn consult_allows_repeat(request: &str) -> bool {
    static REPEAT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(again|another|different|new|repeat|second)\b").unwrap()
    });
    REPEAT.is_match(request)
}

fn dispatch_consult(
    args: &Value,
    call_id: &str,
    tx: &mpsc::Sender<ChannelMessage>,
    alias: &str,
    meta: &CallMeta,
    out_tx: &mpsc::UnboundedSender<WsMessage>,
    dedupe: &Arc<Mutex<ConsultDedupe>>,
) {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        let _ = out_tx.send(function_call_output(
            call_id,
            &json!({ "error": "query is required" }),
        ));
        let _ = out_tx.send(response_create_empty());
        return;
    }

    // Dedupe SMS/text consults: short-circuit an in-flight or already-completed
    // duplicate so a model that fires the same "text +1555 saying X" twice does
    // not double-send. A request that explicitly asks for another/repeat message
    // bypasses this.
    let dedupe_key = consult_dedupe_key(&query).filter(|_| !consult_allows_repeat(&query));
    if let Some(key) = dedupe_key.as_deref() {
        let mut guard = dedupe.lock();
        if let Some(existing) = guard.pending.get(key).cloned() {
            drop(guard);
            let _ = out_tx.send(function_call_output(
                call_id,
                &json!({
                    "status": "already_running",
                    "existing_tool_call_id": existing,
                    "answer": "You are already handling this same in-call request. Do not call the consult tool again or queue a duplicate post-call action; wait briefly for the existing result.",
                }),
            ));
            let _ = out_tx.send(response_create_empty());
            return;
        }
        if let Some(result) = guard
            .results
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, r)| r.clone())
        {
            drop(guard);
            let _ = out_tx.send(function_call_output(
                call_id,
                &json!({
                    "status": "already_handled",
                    "answer": format!("You already handled this same in-call request: {result}. Do not send it again unless the caller explicitly asks for another, repeat, or different message."),
                }),
            ));
            let _ = out_tx.send(response_create_empty());
            return;
        }
        // Mark this key in flight against the current tool call id.
        guard.pending.insert(key.to_string(), call_id.to_string());
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
        super::now_secs(),
    );
    cm.channel_alias = Some(alias.to_string());
    if let Err(e) = tx.try_send(cm) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            format!("[inkbox] dropped in-call consult dispatch: {e}"),
        );
    }

    // Interim acknowledgement so the caller isn't left in silence.
    let _ = out_tx.send(response_create_instructions(
        "Say only 'One moment.' Do not mention waiting for a lookup.",
    ));

    let out_tx = out_tx.clone();
    let call_id = call_id.to_string();
    let dedupe = dedupe.clone();
    zeroclaw_spawn::spawn!(async move {
        let answer = match tokio::time::timeout(Duration::from_secs(CONSULT_TIMEOUT_SECS), orx)
            .await
        {
            Ok(Ok(ans)) => ans,
            _ => {
                consult_sinks().lock().remove(&id);
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    format!("[inkbox] in-call consult {id} timed out or got no answer; continuing"),
                );
                "I couldn't reach the assistant just now — let's continue.".to_string()
            }
        };
        // Record the completed result and clear the in-flight marker so a later
        // identical consult short-circuits with `already_handled`.
        if let Some(key) = &dedupe_key {
            let mut guard = dedupe.lock();
            guard.results.push((key.clone(), answer.clone()));
            if guard
                .pending
                .get(key)
                .map(|v| v == &call_id)
                .unwrap_or(false)
            {
                guard.pending.remove(key);
            }
        }
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

    // The caller's full contact card, so the post-call turn knows exactly who
    // to email/text without re-resolving or asking.
    let caller_block = match meta
        .contact_card
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        Some(card) => {
            let who = meta.contact_name.as_deref().unwrap_or("the caller");
            format!("\n\nCaller ({who}) contact card:\n{card}")
        }
        None => String::new(),
    };

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
        b.push_str(&caller_block);
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
        b.push_str(&caller_block);
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
        super::now_secs(),
    );
    cm.channel_alias = Some(alias.to_string());
    // A dropped post-call turn loses queued actions / the reflection — don't
    // let that vanish silently.
    if let Err(e) = tx.try_send(cm) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            format!(
                "[inkbox] dropped post-call dispatch for call {}: {e}",
                meta.call_id
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbound() -> CallMeta {
        CallMeta {
            direction: "inbound".into(),
            ..Default::default()
        }
    }

    #[test]
    fn realtime_usable_requires_enabled_and_a_key() {
        assert!(RealtimeConfig::usable(true, "sk-123"));
        assert!(!RealtimeConfig::usable(false, "sk-123")); // disabled
        assert!(!RealtimeConfig::usable(true, "")); // no key
        assert!(!RealtimeConfig::usable(true, "   ")); // whitespace-only key
    }

    #[test]
    fn instructions_first_person_known_caller_with_card() {
        let m = CallMeta {
            agent_handle: "zero-claw".into(),
            agent_email: Some("zero@inkbox.ai".into()),
            contact_name: Some("Ada Lovelace".into()),
            contact_card: Some("- Name: Ada Lovelace\n- Emails: ada@inkbox.ai".into()),
            direction: "inbound".into(),
            ..Default::default()
        };
        let s = build_instructions(&m);
        assert!(s.contains("speak in the first person"));
        assert!(s.contains("Caller name: Ada Lovelace."));
        assert!(s.contains("Their full contact card:"));
        assert!(s.contains("Your email identity: zero@inkbox.ai."));
        assert!(!s.contains("No matching contact record"));
    }

    #[test]
    fn instructions_unknown_caller_and_caller_sentinel() {
        assert!(build_instructions(&inbound()).contains("No matching contact record is loaded"));
        // the literal "caller" sentinel is treated as unknown
        let m = CallMeta {
            contact_name: Some("caller".into()),
            ..inbound()
        };
        assert!(build_instructions(&m).contains("No matching contact record is loaded"));
    }

    #[test]
    fn instructions_outbound_includes_purpose() {
        let m = CallMeta {
            direction: "outbound".into(),
            purpose: Some("confirm the order".into()),
            ..Default::default()
        };
        let s = build_instructions(&m);
        assert!(s.contains("This is an outbound call you placed. Purpose: confirm the order"));
        assert!(s.contains("do not open with a generic offer to help"));
    }

    #[test]
    fn greeting_uses_first_name_inbound_else_there() {
        let m = CallMeta {
            contact_name: Some("Ada Lovelace".into()),
            ..inbound()
        };
        assert!(build_greeting(&m).contains("Hi Ada,"));
        assert!(build_greeting(&inbound()).contains("Hi there,"));
    }

    #[test]
    fn greeting_outbound_prefers_opening_then_purpose() {
        let opening = CallMeta {
            direction: "outbound".into(),
            opening: Some("Hi, calling about your appointment.".into()),
            ..Default::default()
        };
        assert!(build_greeting(&opening).contains("Hi, calling about your appointment."));
        let purpose = CallMeta {
            direction: "outbound".into(),
            purpose: Some("reschedule".into()),
            ..Default::default()
        };
        assert!(build_greeting(&purpose).contains("explaining why you are calling"));
    }

    fn sample_contact() -> inkbox::contacts::types::Contact {
        serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "preferred_name": "Ada",
            "given_name": "Ada",
            "family_name": "Lovelace",
            "company_name": "Globex",
            "job_title": "Engineer",
            "emails": [{ "value": "ada@inkbox.ai", "is_primary": true, "label": "work" }],
            "phones": [{ "value_e164": "+15551230000", "is_primary": true, "label": "mobile" }],
            "notes": "VIP",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .expect("valid contact json")
    }

    #[test]
    fn contact_display_name_prefers_preferred_name() {
        assert_eq!(
            contact_display_name(&sample_contact()).as_deref(),
            Some("Ada")
        );
    }

    #[test]
    fn render_contact_card_includes_all_populated_fields() {
        let card = render_contact_card(&sample_contact());
        assert!(card.contains("- Name: Ada Lovelace"));
        assert!(card.contains("- Company: Globex (Engineer)"));
        assert!(card.contains("ada@inkbox.ai"));
        assert!(card.contains("+15551230000"));
        assert!(card.contains("- Notes: VIP"));
    }
}
