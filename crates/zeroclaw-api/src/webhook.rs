//! Cross-component wiring for routing inbound platform webhooks into a WASM
//! channel plugin.
//!
//! A webhook-based channel (WhatsApp Cloud, LINE, Slack Events API, …) already
//! sends over `wasi:http`; it only lacks inbound, which arrives as a platform
//! POST to a host gateway endpoint. The gateway (which must NOT depend on
//! `zeroclaw-plugins`/wasmtime) and the channel orchestrator share a
//! [`PluginWebhookRegistry`]: a plugin channel registers the path it serves, the
//! gateway hands a received [`RawWebhook`] to that path's sink, and the plugin
//! decodes + authenticates it inside its own `parse-webhook` export.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{mpsc, oneshot};

/// Cancellation signal for one gateway-owned plugin webhook request.
pub type WebhookCancellation = tokio_util::sync::CancellationToken;

/// A live handle to the gateway's canonical idempotency store.
///
/// The plugin worker calls [`Self::reserve`] only after the guest has
/// authenticated and parsed a stable message ID. A reservation remains in the
/// gateway store after successful channel delivery and is rolled back when
/// delivery fails or the request is cancelled before handoff.
#[derive(Clone)]
pub struct WebhookIdempotency {
    reserve: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    rollback: Arc<dyn Fn(&str) + Send + Sync>,
}

impl WebhookIdempotency {
    pub fn new(
        reserve: impl Fn(&str) -> bool + Send + Sync + 'static,
        rollback: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            reserve: Arc::new(reserve),
            rollback: Arc::new(rollback),
        }
    }

    /// Reserve a parsed stable message ID. Returns `false` for a duplicate.
    #[must_use]
    pub fn reserve(&self, message_id: &str) -> bool {
        (self.reserve)(message_id)
    }

    /// Remove a reservation whose message never reached the channel queue.
    pub fn rollback(&self, message_id: &str) {
        (self.rollback)(message_id);
    }
}

/// The reserved `channel` value a plugin returns from `parse-webhook` to make the
/// gateway reply 200 with a custom body instead of enqueuing a message. Used for
/// verification handshakes that echo a challenge in the HTTP response — Slack
/// `url_verification` (POST) and WhatsApp/wecom `hub.challenge` (GET). A
/// `parse-webhook` that returns a single message whose `channel` equals this
/// sentinel is answered with that message's `content` as the response body when
/// it is no larger than [`MAX_WEBHOOK_RESPONSE_BODY_BYTES`]; an oversized body
/// is rejected. The message is not enqueued. This keeps the challenge feature
/// additive — no `channel`-interface signature change, so existing plugins need
/// no rebuild.
pub const WEBHOOK_REPLY_CHANNEL: &str = "__webhook_reply__";

/// Maximum UTF-8 byte length of a plugin-supplied webhook response body.
///
/// Verification challenges are short opaque values. Keeping this limit at the
/// shared host/gateway boundary prevents a guest's much larger linear-memory
/// allowance from becoming an equally large public HTTP response.
pub const MAX_WEBHOOK_RESPONSE_BODY_BYTES: usize = 4 * 1024;

/// A raw inbound webhook the gateway received on `/plugin/<path>`, plus a
/// one-shot the plugin side resolves so the HTTP handler can pick a status code.
pub struct RawWebhook {
    /// HTTP method, upper-cased (`"GET"` | `"POST"`). Surfaced to the plugin as
    /// the reserved `x-webhook-method` header so it can handle GET verification.
    pub method: String,
    /// Raw query string (no leading `?`; `""` when none). Surfaced as the
    /// reserved `x-webhook-query` header — carries e.g. `hub.challenge`.
    pub query: String,
    /// Header names (lower-cased) → values, as received.
    pub headers: Vec<(String, String)>,
    /// Exact received body bytes.
    pub body: Vec<u8>,
    /// Request-lifetime cancellation owned by the gateway. The plugin worker
    /// observes this same token while instantiating the disposable store and
    /// executing `parse-webhook`, so an HTTP timeout or dropped handler cancels
    /// the actual component call instead of only abandoning its reply.
    pub cancellation: WebhookCancellation,
    /// Resolver callbacks into the gateway's canonical idempotency store. The
    /// plugin worker uses the authenticated, parsed message ID; it never trusts
    /// a caller-supplied idempotency header before guest authentication.
    pub idempotency: Option<WebhookIdempotency>,
    /// Resolved once the plugin has decoded (or rejected) the webhook. `Ok(Ack)`
    /// → 200 empty; `Ok(Body(s))` → 200 with `s` (a challenge echo);
    /// `Err(reject)` → the reject's status.
    pub reply: oneshot::Sender<Result<WebhookOutcome, WebhookReject>>,
}

/// A successful webhook outcome — how the gateway answers a 200.
#[derive(Debug, Clone)]
pub enum WebhookOutcome {
    /// 200 with an empty body (events accepted / enqueued — the default).
    Ack,
    /// 200 with this exact body when its UTF-8 byte length is no larger than
    /// [`MAX_WEBHOOK_RESPONSE_BODY_BYTES`]: a verification-handshake echo
    /// (Slack `url_verification` challenge, WhatsApp `hub.challenge`).
    /// Oversized values are rejected with a fixed public 502 response.
    Body(String),
}

/// Why a webhook was rejected — drives the gateway's HTTP status.
#[derive(Debug, Clone)]
pub enum WebhookReject {
    /// The plugin's authenticity check failed → the gateway replies 401.
    Unauthorized(String),
    /// The plugin could not decode the payload → the gateway replies 400.
    BadRequest(String),
    /// The plugin produced an invalid public response → the gateway replies
    /// with a fixed 502 response that contains no guest-controlled detail.
    InvalidResponse,
    /// The request lifetime ended before plugin decoding completed → 504.
    Timeout,
}

/// Path → sink registry, shared (`Arc`) between the gateway and the channel
/// orchestrator. Restart-safe: rebuilt each daemon iteration. Not a duplicate of
/// channel config — it is a materialized routing view owned by the running
/// daemon, keyed on the path a plugin declares at load time.
#[derive(Default, Clone)]
pub struct PluginWebhookRegistry {
    routes: Arc<Mutex<HashMap<String, mpsc::Sender<RawWebhook>>>>,
}

impl PluginWebhookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin channel's webhook sink under `path`. First writer wins:
    /// a duplicate path is rejected (returns `false`) so two plugins can't claim
    /// one route.
    pub fn insert(&self, path: String, sink: mpsc::Sender<RawWebhook>) -> bool {
        let mut routes = self.lock_routes();
        if routes.contains_key(&path) {
            return false;
        }
        routes.insert(path, sink);
        true
    }

    /// The sink for `path`, if any plugin serves it.
    pub fn get(&self, path: &str) -> Option<mpsc::Sender<RawWebhook>> {
        self.lock_routes().get(path).cloned()
    }

    fn lock_routes(&self) -> MutexGuard<'_, HashMap<String, mpsc::Sender<RawWebhook>>> {
        match self.routes.lock() {
            Ok(routes) => routes,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PluginWebhookRegistry;

    #[test]
    fn registry_recovers_after_a_poisoned_lock() {
        let registry = PluginWebhookRegistry::new();
        let poison_target = registry.clone();
        let poisoner = std::thread::spawn(move || {
            let routes = poison_target
                .routes
                .lock()
                .expect("test obtains registry lock");
            assert!(routes.is_empty());
            panic!("poison registry for recovery test");
        });
        assert!(poisoner.join().is_err());

        let (sink, receiver) = tokio::sync::mpsc::channel(1);
        assert!(registry.insert("fixture".to_string(), sink));
        assert!(registry.get("fixture").is_some());
        drop(receiver);
    }
}
