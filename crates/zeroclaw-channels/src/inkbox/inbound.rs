//! Loopback HTTP server for tunnel-forwarded Inkbox inbound traffic.
//!
//! The Inkbox tunnel forwards inbound webhook POSTs (and the call-media
//! WebSocket) to this server. Each webhook is HMAC-verified against the
//! identity's signing key, then mapped to a [`ChannelMessage`] with a tagged
//! `reply_target` the channel's `send` understands.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::Router;
use serde_json::Value;
use tokio::sync::mpsc;
use zeroclaw_api::channel::ChannelMessage;

/// Shared handler state. Cloned per request by axum's `State` extractor, so
/// every field is cheap to clone (the `tx` is an mpsc sender).
#[derive(Clone)]
pub struct AppState {
    /// Inbound message sink consumed by the orchestrator.
    pub tx: mpsc::Sender<ChannelMessage>,
    /// Webhook signing key (`whsec_...`) for HMAC verification.
    pub signing_key: String,
    /// ZeroClaw channel alias, stamped onto every inbound message.
    pub alias: String,
    /// Realtime bridge config for calls; `None` uses Inkbox STT/TTS.
    pub realtime: Option<super::realtime::RealtimeConfig>,
    /// Inkbox client + identity handle, for the realtime bridge to resolve the
    /// agent's own identity (so the model speaks as ZeroClaw with real contacts).
    pub inkbox: std::sync::Arc<inkbox::Inkbox>,
    pub identity: String,
}

/// Build the loopback router: the call-media WebSocket on its fixed path, and
/// a catch-all fallback that treats every other request as a webhook (the
/// tunnel preserves whatever path Inkbox's subscription posts to).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/phone/media/ws", get(super::voice::ws_handler))
        .fallback(webhook)
        .with_state(state)
}

/// Webhook entry point. Verifies the signature, parses the event, and forwards
/// any inbound message to the orchestrator. Always returns quickly so the
/// tunnel's response deadline is met.
async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    // Flatten headers to the `HashMap<String, String>` the SDK verifier wants.
    let mut header_map = HashMap::with_capacity(headers.len());
    for (k, v) in headers.iter() {
        if let Ok(s) = v.to_str() {
            header_map.insert(k.as_str().to_string(), s.to_string());
        }
    }

    // Drop anything that does not carry a valid Inkbox signature.
    match inkbox::signing_keys::verify_webhook(&body, &header_map, &state.signing_key) {
        Ok(true) => {}
        _ => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "[inkbox] dropping webhook with invalid or missing signature",
            );
            return StatusCode::UNAUTHORIZED;
        }
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    if let Some(msg) = map_event(&payload, &state.alias) {
        // A full inbound queue should not wedge the tunnel; drop on backpressure.
        let _ = state.tx.try_send(msg);
    }
    StatusCode::OK
}

/// Seconds since the Unix epoch, for the `ChannelMessage` timestamp.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolve a human display name + optional Inkbox id for the remote party from
/// a webhook `data` block. Inkbox resolves the sender against the org address
/// book (`contacts: [{id, name}]`) and identity graph
/// (`agent_identities: [{id, agent_handle, display_name}]`); surfacing it lets
/// the agent know *who* it's talking to instead of a bare number/address.
///
/// Returns `(label, inkbox_id)` — `label` falls back to `fallback` (the raw
/// number/address) when nothing resolved.
fn resolve_party(data: Option<&Value>, fallback: &str) -> (String, Option<String>) {
    let first = |key: &str| -> Option<&Value> {
        data?.get(key).and_then(Value::as_array).and_then(|a| a.first())
    };
    if let Some(c) = first("contacts") {
        if let Some(name) = c.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            return (name.to_string(), c.get("id").and_then(Value::as_str).map(str::to_string));
        }
    }
    if let Some(ai) = first("agent_identities") {
        let name = ai
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| ai.get("agent_handle").and_then(Value::as_str))
            .filter(|s| !s.is_empty());
        if let Some(name) = name {
            return (name.to_string(), ai.get("id").and_then(Value::as_str).map(str::to_string));
        }
    }
    (fallback.to_string(), None)
}

/// Prepend a one-line sender marker so the resolved identity is explicit in the
/// agent's prompt, and the contact id is available for follow-up tools.
fn with_party_marker(label: &str, addr: &str, contact_id: Option<&str>, body: &str) -> String {
    let header = match (label != addr, contact_id) {
        (true, Some(id)) => format!("[from {label} <{addr}> · inkbox contact_id={id}]"),
        (true, None) => format!("[from {label} <{addr}>]"),
        (false, _) => format!("[from {addr}]"),
    };
    format!("{header}\n{body}")
}

/// Map an Inkbox webhook payload to a [`ChannelMessage`]. Returns `None` for
/// events we don't surface inbound (delivery receipts, lifecycle events; calls
/// are handled over the WebSocket). The `reply_target` is tagged
/// `"<mode>:<id>"` so `InkboxChannel::send` can route the agent's reply.
fn map_event(payload: &Value, alias: &str) -> Option<ChannelMessage> {
    let ts = now_secs();
    let with_alias = |mut cm: ChannelMessage| {
        cm.channel_alias = Some(alias.to_string());
        cm
    };

    match payload.get("event_type").and_then(Value::as_str) {
        // Inbound email → reply by email address.
        Some("message.received") => {
            let m = payload.pointer("/data/message")?;
            let from = m.get("from_address").and_then(Value::as_str)?.to_string();
            // Prefer the RFC 5322 Message-ID so a reply threads correctly:
            // `SendMessage::reply_to` stamps this into `in_reply_to`, which the
            // send path passes as `in_reply_to_message_id`. Fall back to the
            // Inkbox row id when the header is absent.
            let id = m
                .get("message_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| m.get("id").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            let body = m.get("snippet").and_then(Value::as_str).unwrap_or("").to_string();
            let (label, contact_id) = resolve_party(payload.get("data"), &from);
            let content = with_party_marker(&label, &from, contact_id.as_deref(), &body);
            let mut cm = ChannelMessage::new(
                id,
                label,
                format!("email:{from}"),
                content,
                "inkbox",
                ts,
            );
            cm.subject = m.get("subject").and_then(Value::as_str).map(str::to_string);
            Some(with_alias(cm))
        }
        // Inbound SMS/MMS → reply into the conversation (fall back to remote #).
        Some("text.received") => {
            let t = payload.pointer("/data/text_message")?;
            let remote = t
                .get("remote_phone_number")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let text = t.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            let id = t.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            // Reply into the existing conversation when Inkbox gives us one;
            // otherwise reply to the bare remote number via `to`. A phone
            // number is NOT a conversation id, so it must route through the
            // `smsto:` arm (send_text `to=`) rather than `conversation_id=`.
            let reply_target = match t
                .get("conversation_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                Some(cid) => format!("sms:{cid}"),
                None => format!("smsto:{remote}"),
            };
            let (label, contact_id) = resolve_party(payload.get("data"), &remote);
            let content = with_party_marker(&label, &remote, contact_id.as_deref(), &text);
            Some(with_alias(ChannelMessage::new(
                id,
                label,
                reply_target,
                content,
                "inkbox",
                ts,
            )))
        }
        // Inbound iMessage → reply into the conversation.
        Some("imessage.received") => {
            let m = payload.pointer("/data/message")?;
            let remote = m
                .get("remote_number")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let content = m.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            let id = m.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let convo = m
                .get("conversation_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let (label, contact_id) = resolve_party(payload.get("data"), &remote);
            let enriched = with_party_marker(&label, &remote, contact_id.as_deref(), &content);
            Some(with_alias(ChannelMessage::new(
                id,
                label,
                format!("imessage:{convo}"),
                enriched,
                "inkbox",
                ts,
            )))
        }
        _ => None,
    }
}
