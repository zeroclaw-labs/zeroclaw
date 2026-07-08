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
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

/// A raw inbound webhook the gateway received on `/plugin/<path>`, plus a
/// one-shot the plugin side resolves so the HTTP handler can pick a status code.
pub struct RawWebhook {
    /// Header names (lower-cased) → values, as received.
    pub headers: Vec<(String, String)>,
    /// Exact received body bytes.
    pub body: Vec<u8>,
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
        let mut routes = self.routes.lock().expect("webhook registry poisoned");
        if routes.contains_key(&path) {
            return false;
        }
        routes.insert(path, sink);
        true
    }

    /// The sink for `path`, if any plugin serves it.
    pub fn get(&self, path: &str) -> Option<mpsc::Sender<RawWebhook>> {
        self.routes
            .lock()
            .expect("webhook registry poisoned")
            .get(path)
            .cloned()
    }
}
