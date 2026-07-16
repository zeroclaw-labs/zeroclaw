//! Loopback HTTP server for tunnel-forwarded Inkbox inbound traffic.
//!
//! The Inkbox tunnel forwards inbound webhook POSTs (and the call-media
//! WebSocket) to this server. Each webhook is HMAC-verified against the
//! identity's signing key, then mapped to a [`ChannelMessage`] with a tagged
//! `reply_target` the channel's `send` understands.

use std::collections::HashMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use zeroclaw_api::channel::ChannelMessage;

/// Shared handler state. Cloned per request by axum's `State` extractor, so
/// every field is cheap to clone (the `tx` is an mpsc sender).
#[derive(Clone)]
pub struct AppState {
    /// Inbound message sink consumed by the orchestrator.
    pub tx: mpsc::Sender<ChannelMessage>,
    /// Delivery-failure retry loop (shared with the channel's send path):
    /// delivery webhooks draw down / reset its budget.
    pub failure: std::sync::Arc<super::delivery_failure::FailureTracker>,
    /// Webhook signing key (`whsec_...`) for HMAC verification.
    pub signing_key: String,
    /// ZeroClaw channel alias, stamped onto every inbound message.
    pub alias: String,
    /// Tunnel public host (e.g. `abc.inkbox.ai`), used to build the call-media
    /// WS URL we hand back from the incoming-call webhook with `?call_id=`.
    pub public_host: String,
}

/// Build the loopback router: the call-media WebSocket on its fixed path, and
/// a catch-all fallback that treats every other request as a webhook (the
/// tunnel preserves whatever path Inkbox's subscription posts to).
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/phone/media/ws", get(super::voice::ws_handler))
        .route("/incoming-call", post(incoming_call))
        .fallback(webhook)
        .with_state(state)
}

/// Incoming-call webhook. With the phone number set to
/// `incoming_call_action="webhook"`, Inkbox calls this synchronously when a
/// call arrives and uses our response to bridge the audio. We answer and hand
/// back the call-media WS URL stamped with `?call_id=<id>`, which the media
/// handler binds against the signed call context on upgrade.
///
/// Fails CLOSED at the trust boundary: Inkbox signs this webhook (the same V2
/// `X-Inkbox-*` scheme `webhook` verifies), so an unsigned/forged request is
/// rejected rather than answered — answering one would let an attacker drive a
/// call leg. The `call_id` is validated as a UUID before it is trusted in the
/// WS URL.
async fn incoming_call(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let mut header_map = HashMap::with_capacity(headers.len());
    for (k, v) in headers.iter() {
        if let Ok(s) = v.to_str() {
            header_map.insert(k.as_str().to_string(), s.to_string());
        }
    }
    if !matches!(
        inkbox::signing_keys::verify_webhook(&body, &header_map, &state.signing_key),
        Ok(true)
    ) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "[inkbox] rejecting incoming-call webhook with invalid or missing signature",
        );
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "[inkbox] incoming-call webhook body was not valid JSON; declining",
            );
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    // The Inkbox call id — flat on the payload, with a /data fallback.
    let call_id = payload
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/data/id").and_then(Value::as_str))
        .unwrap_or("");
    if uuid::Uuid::parse_str(call_id).is_err() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "[inkbox] incoming-call webhook has no valid call id; declining",
        );
        return StatusCode::BAD_REQUEST.into_response();
    }
    let ws = format!(
        "wss://{}/phone/media/ws?call_id={call_id}",
        state.public_host
    );

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        format!("[inkbox] answering incoming call (call_id={call_id})"),
    );
    axum::Json(json!({ "action": "answer", "client_websocket_url": ws })).into_response()
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

    // Feed the delivery-failure retry loop first: outbound failure events wake
    // the agent, delivered receipts and fresh inbounds reset its budget.
    state.failure.observe_event(&payload);

    if let Some(msg) = map_event(&payload, &state.alias) {
        // Sessions are sender-scoped: stash the resolved label so a later
        // delivery-failure wake-up for this reply target joins this session.
        state
            .failure
            .remember_sender(&msg.reply_target, &msg.sender);
        // A full inbound queue must not wedge the tunnel, so we drop on
        // backpressure — but a silently lost inbound message is exactly the kind
        // of failure worth seeing, so log it.
        if let Err(e) = state.tx.try_send(msg) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                format!("[inkbox] dropped inbound message on backpressure: {e}"),
            );
        }
    }
    StatusCode::OK
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
        data?
            .get(key)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
    };
    if let Some(c) = first("contacts")
        && let Some(name) = c
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    {
        return (
            name.to_string(),
            c.get("id").and_then(Value::as_str).map(str::to_string),
        );
    }
    if let Some(ai) = first("agent_identities") {
        let name = ai
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| ai.get("agent_handle").and_then(Value::as_str))
            .filter(|s| !s.is_empty());
        if let Some(name) = name {
            return (
                name.to_string(),
                ai.get("id").and_then(Value::as_str).map(str::to_string),
            );
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
    let ts = super::now_secs();
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
            let body = m
                .get("snippet")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let (label, contact_id) = resolve_party(payload.get("data"), &from);
            let content = with_party_marker(&label, &from, contact_id.as_deref(), &body);
            let mut cm =
                ChannelMessage::new(id, label, format!("email:{from}"), content, "inkbox", ts);
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
            let text = t
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let id = t
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
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
            let content = m
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let id = m
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_event_maps_to_email_target_and_threads_by_message_id() {
        let payload = json!({
            "event_type": "message.received",
            "data": {
                "message": {
                    "from_address": "alice@example.com",
                    "message_id": "<msg-123@mail>",
                    "id": "row-9",
                    "snippet": "hi there",
                    "subject": "Hello"
                },
                "contacts": [{ "id": "c1", "name": "Alice" }]
            }
        });
        let cm = map_event(&payload, "zc").expect("email event maps");
        assert_eq!(cm.reply_target, "email:alice@example.com");
        assert_eq!(cm.id, "<msg-123@mail>"); // Message-ID preferred over row id
        assert_eq!(cm.sender, "Alice");
        assert_eq!(cm.subject.as_deref(), Some("Hello"));
        assert_eq!(cm.channel_alias.as_deref(), Some("zc"));
        assert!(cm.content.contains("Alice"));
    }

    #[test]
    fn email_falls_back_to_row_id_without_message_id() {
        let payload = json!({
            "event_type": "message.received",
            "data": { "message": { "from_address": "a@b.com", "id": "row-9", "snippet": "" } }
        });
        assert_eq!(map_event(&payload, "zc").unwrap().id, "row-9");
    }

    #[test]
    fn sms_with_conversation_uses_sms_target_else_smsto() {
        let with_conv = json!({
            "event_type": "text.received",
            "data": { "text_message": { "remote_phone_number": "+15551230000", "text": "yo", "id": "t1", "conversation_id": "conv-7" } }
        });
        assert_eq!(
            map_event(&with_conv, "zc").unwrap().reply_target,
            "sms:conv-7"
        );

        let no_conv = json!({
            "event_type": "text.received",
            "data": { "text_message": { "remote_phone_number": "+15551230000", "text": "yo", "id": "t1" } }
        });
        assert_eq!(
            map_event(&no_conv, "zc").unwrap().reply_target,
            "smsto:+15551230000"
        );
    }

    #[test]
    fn imessage_maps_to_conversation_target() {
        let payload = json!({
            "event_type": "imessage.received",
            "data": { "message": { "remote_number": "+15551230000", "content": "hi", "id": "m1", "conversation_id": "ic-2" } }
        });
        assert_eq!(
            map_event(&payload, "zc").unwrap().reply_target,
            "imessage:ic-2"
        );
    }

    #[test]
    fn unknown_and_lifecycle_events_are_dropped() {
        assert!(map_event(&json!({ "event_type": "message.delivered" }), "zc").is_none());
        assert!(map_event(&json!({}), "zc").is_none());
    }

    #[test]
    fn resolve_party_prefers_contacts_then_identities_then_fallback() {
        let contacts = json!({ "contacts": [{ "id": "c1", "name": "Alice" }] });
        assert_eq!(resolve_party(Some(&contacts), "x@y").0, "Alice");

        let identities = json!({ "agent_identities": [{ "id": "a1", "display_name": "Bot" }] });
        assert_eq!(
            resolve_party(Some(&identities), "x@y"),
            ("Bot".into(), Some("a1".into()))
        );

        let empty = json!({ "contacts": [], "agent_identities": [] });
        assert_eq!(resolve_party(Some(&empty), "x@y").0, "x@y");
        assert_eq!(resolve_party(None, "x@y"), ("x@y".to_string(), None));
    }

    #[test]
    fn party_marker_shapes() {
        assert_eq!(
            with_party_marker("Alice", "a@b", Some("c1"), "body"),
            "[from Alice <a@b> · inkbox contact_id=c1]\nbody"
        );
        assert_eq!(
            with_party_marker("Alice", "a@b", None, "body"),
            "[from Alice <a@b>]\nbody"
        );
        // label == addr collapses to a single token
        assert_eq!(
            with_party_marker("a@b", "a@b", None, "body"),
            "[from a@b]\nbody"
        );
    }
}
