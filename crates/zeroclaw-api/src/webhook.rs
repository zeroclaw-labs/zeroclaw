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

/// A raw inbound webhook the gateway received on `/plugin/<path>`, plus a
/// one-shot the plugin side resolves so the HTTP handler can pick a status code.
pub struct RawWebhook {
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
    /// Resolved once the plugin has decoded (or rejected) the webhook: `Ok` →
    /// 200, `Err(reject)` → the reject's status.
    pub reply: oneshot::Sender<Result<(), WebhookReject>>,
}

/// Why a webhook was rejected — drives the gateway's HTTP status.
#[derive(Debug, Clone)]
pub enum WebhookReject {
    /// The plugin's authenticity check failed → the gateway replies 401.
    Unauthorized(String),
    /// The plugin could not decode the payload → the gateway replies 400.
    BadRequest(String),
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
