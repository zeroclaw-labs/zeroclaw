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
//! to push the text onto this socket as TTS. The raw-audio mode
//! (`x-use-inkbox-*: false`) is intentionally not implemented; the Inkbox
//! STT/TTS path needs no external model.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{Value, json};
use zeroclaw_api::channel::ChannelMessage;

use super::inbound::AppState;

/// Opening line spoken on pickup.
const STT_GREETING: &str = "Hi there, how can I help?";

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

/// Accept-window for the signed upgrade timestamp, shared with the webhook
/// handlers so every signed surface enforces the same freshness contract.
const UPGRADE_TIMESTAMP_TOLERANCE_SECS: i64 = super::inbound::SIGNED_REQUEST_TOLERANCE_SECS;

/// One accepted upgrade per call: Inkbox opens exactly one media socket per
/// call and never reconnects it, so a second upgrade presenting the same
/// (validly signed) call id is a captured-header replay. Entries only need to
/// outlive the timestamp window; they are pruned on insert.
static ACCEPTED_CALL_IDS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn accepted_call_ids() -> &'static Mutex<HashMap<String, Instant>> {
    ACCEPTED_CALL_IDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Authenticate a call-media WebSocket upgrade before it is accepted.
///
/// The tunnel's public host is reachable by anyone on the internet — its TLS
/// authenticates the SDK<->edge channel, not the requests flowing through it.
/// Inkbox signs the media upgrade the same way it signs webhooks: the
/// `X-Call-Context` header carries `{"call_id",...}` JSON and the
/// `X-Inkbox-*` headers carry an HMAC over those bytes, keyed by the
/// identity's signing key. This gate runs before the 101, so an
/// unauthenticated socket never reaches the bridge or speaks a caller turn.
/// Fails CLOSED, like the webhook handler.
///
/// # Arguments
/// * `headers` - the upgrade request's headers.
/// * `params` - the upgrade URL's query parameters (`call_id` when inbound).
/// * `signing_key` - the identity's webhook signing key.
///
/// # Returns
/// `Ok(call_id)` — the *signed* call id, the canonical identity of this leg —
/// or `Err((status, reason))` with the rejection to return.
fn authenticate_upgrade(
    headers: &HeaderMap,
    params: &HashMap<String, String>,
    signing_key: &str,
) -> Result<String, (StatusCode, &'static str)> {
    let reject = |why: &'static str| -> (StatusCode, &'static str) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            format!("[inkbox] rejecting call-media upgrade: {why}"),
        );
        (StatusCode::UNAUTHORIZED, why)
    };

    // Flatten to the map shape the SDK verifier wants (it lowercases keys).
    let mut header_map = HashMap::with_capacity(headers.len());
    for (k, v) in headers.iter() {
        if let Ok(s) = v.to_str() {
            header_map.insert(k.as_str().to_string(), s.to_string());
        }
    }

    // The signed payload is the X-Call-Context header value.
    let Some(context_raw) = header_map.get("x-call-context").filter(|c| !c.is_empty()) else {
        return Err(reject("missing X-Call-Context"));
    };
    if !matches!(
        inkbox::signing_keys::verify_webhook(context_raw.as_bytes(), &header_map, signing_key),
        Ok(true)
    ) {
        return Err(reject("invalid or missing signature"));
    }

    // The timestamp is covered by the HMAC; enforce freshness so captured
    // headers age out.
    let ts = header_map
        .get("x-inkbox-timestamp")
        .and_then(|t| t.parse::<i64>().ok());
    let now = i64::try_from(super::now_secs()).unwrap_or(i64::MAX);
    match ts {
        Some(ts) if (now - ts).abs() <= UPGRADE_TIMESTAMP_TOLERANCE_SECS => {}
        _ => return Err(reject("stale or missing timestamp")),
    }

    // The signed context names the call this socket belongs to; that binding
    // is what a query parameter alone cannot prove.
    let signed_call_id = serde_json::from_str::<Value>(context_raw)
        .ok()
        .and_then(|v| {
            v.get("call_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
    let Some(signed_call_id) = signed_call_id else {
        return Err(reject("call context carries no call_id"));
    };
    if let Some(query_call_id) = params.get("call_id").filter(|c| !c.is_empty())
        && query_call_id != &signed_call_id
    {
        return Err(reject("call_id does not match the signed call context"));
    }

    // Replay: one media socket per call, ever.
    {
        let mut accepted = accepted_call_ids().lock();
        let now = Instant::now();
        accepted.retain(|_, at| {
            now.duration_since(*at)
                <= Duration::from_secs(2 * UPGRADE_TIMESTAMP_TOLERANCE_SECS as u64)
        });
        if accepted.contains_key(&signed_call_id) {
            return Err(reject("replayed upgrade for an already-connected call"));
        }
        accepted.insert(signed_call_id.clone(), now);
    }

    Ok(signed_call_id)
}

/// Upgrade the call-media connection, asking Inkbox to run STT + TTS, and hand
/// the socket to the bridge loop.
pub(crate) async fn ws_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Authenticate before the upgrade and before any frame is processed.
    let verified_call_id = match authenticate_upgrade(&headers, &params, &state.signing_key) {
        Ok(call_id) => call_id,
        Err(rejection) => return rejection.into_response(),
    };
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        format!("[inkbox] call-media upgrade authenticated (call_id={verified_call_id})"),
    );

    let mut resp = ws
        .on_upgrade(move |socket| bridge(socket, state))
        .into_response();
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
                        if !speak_turn(&mut sink, "greeting", STT_GREETING).await {
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
                            super::now_secs(),
                        );
                        cm.channel_alias = Some(state.alias.clone());
                        // Drop on backpressure rather than wedge the call leg, but
                        // log it — a dropped caller turn shouldn't vanish silently.
                        if let Err(e) = state.tx.try_send(cm) {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                                format!("[inkbox] dropped caller transcript (conn={conn_id}): {e}"),
                            );
                        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    const KEY: &str = "test-signing-key";

    /// Sign an upgrade the way the Inkbox server does: HMAC-SHA256 over
    /// `"{request_id}.{timestamp}." + X-Call-Context bytes`.
    fn signed_headers(call_id: &str, timestamp: i64, key: &str) -> HeaderMap {
        let context = format!(
            "{{\"call_id\":\"{call_id}\",\"direction\":\"inbound\",\"phone_number\":null}}"
        );
        let request_id = format!("req-{call_id}");
        let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(format!("{request_id}.{timestamp}.").as_bytes());
        mac.update(context.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert("x-call-context", context.parse().unwrap());
        headers.insert("x-inkbox-request-id", request_id.parse().unwrap());
        headers.insert("x-inkbox-timestamp", timestamp.to_string().parse().unwrap());
        headers.insert(
            "x-inkbox-signature",
            format!("sha256={sig}").parse().unwrap(),
        );
        headers
    }

    fn now() -> i64 {
        i64::try_from(super::super::now_secs()).unwrap()
    }

    fn no_params() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn valid_signed_upgrade_is_accepted_and_yields_the_signed_call_id() {
        let headers = signed_headers("call-ok", now(), KEY);
        let params = HashMap::from([("call_id".to_string(), "call-ok".to_string())]);
        assert_eq!(
            authenticate_upgrade(&headers, &params, KEY).ok(),
            Some("call-ok".to_string()),
        );
    }

    #[test]
    fn missing_context_and_forged_signature_are_rejected() {
        // No auth headers at all.
        assert!(authenticate_upgrade(&HeaderMap::new(), &no_params(), KEY).is_err());
        // Signed with the wrong key.
        let headers = signed_headers("call-forged", now(), "attacker-key");
        assert!(authenticate_upgrade(&headers, &no_params(), KEY).is_err());
        // Valid signature, tampered context.
        let mut headers = signed_headers("call-tampered", now(), KEY);
        headers.insert(
            "x-call-context",
            "{\"call_id\":\"other-call\"}".parse().unwrap(),
        );
        assert!(authenticate_upgrade(&headers, &no_params(), KEY).is_err());
    }

    #[test]
    fn stale_timestamp_is_rejected() {
        let headers = signed_headers(
            "call-stale",
            now() - UPGRADE_TIMESTAMP_TOLERANCE_SECS - 60,
            KEY,
        );
        assert!(authenticate_upgrade(&headers, &no_params(), KEY).is_err());
    }

    #[test]
    fn query_call_id_must_match_the_signed_context() {
        let headers = signed_headers("call-bound", now(), KEY);
        let params = HashMap::from([("call_id".to_string(), "someone-elses-call".to_string())]);
        assert!(authenticate_upgrade(&headers, &params, KEY).is_err());
    }

    #[test]
    fn replayed_upgrade_for_the_same_call_is_rejected() {
        let headers = signed_headers("call-replay", now(), KEY);
        assert!(authenticate_upgrade(&headers, &no_params(), KEY).is_ok());
        // Same captured headers presented again: valid HMAC, fresh enough,
        // but the call already has its one socket.
        assert!(authenticate_upgrade(&headers, &no_params(), KEY).is_err());
    }

    /// Route-level proof: an unauthenticated socket never reaches a bridge —
    /// the router answers 401 instead of 101 — and a signed upgrade completes.
    #[tokio::test]
    async fn media_route_rejects_unauthenticated_upgrades() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let app = super::super::inbound::router(super::super::inbound::AppState {
            tx,
            failure: std::sync::Arc::new(super::super::delivery_failure::FailureTracker::new("zc")),
            signing_key: KEY.to_string(),
            alias: "zc".to_string(),
            public_host: "example.test".to_string(),
            request_dedup: std::sync::Arc::default(),
        });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        zeroclaw_spawn::spawn!(axum::serve(listener, app).into_future());

        let request = |auth: Option<&HeaderMap>| {
            let mut req = String::from("GET /phone/media/ws HTTP/1.1\r\n");
            req.push_str(&format!("Host: {addr}\r\n"));
            req.push_str("Connection: Upgrade\r\nUpgrade: websocket\r\n");
            req.push_str("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n");
            req.push_str("Sec-WebSocket-Version: 13\r\n");
            if let Some(headers) = auth {
                for (k, v) in headers {
                    req.push_str(&format!("{k}: {}\r\n", v.to_str().unwrap()));
                }
            }
            req.push_str("\r\n");
            req
        };
        let status_line = |req: String| async move {
            let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
            sock.write_all(req.as_bytes()).await.unwrap();
            let mut buf = [0u8; 64];
            let n = sock.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n])
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        };

        // Anonymous upgrade: rejected before any bridge exists.
        let anon = status_line(request(None)).await;
        assert!(anon.contains("401"), "expected 401, got: {anon}");

        // Signed upgrade: completes the handshake.
        let signed = signed_headers("call-route", now(), KEY);
        let ok = status_line(request(Some(&signed))).await;
        assert!(ok.contains("101"), "expected 101, got: {ok}");
    }
}
