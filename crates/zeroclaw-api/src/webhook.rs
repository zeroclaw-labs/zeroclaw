//! Wasmtime-free wiring for channel-plugin webhook ingress.
//!
//! The gateway and channel supervisor deliberately meet in this API crate so
//! the HTTP boundary does not need to depend on the plugin runtime. A running
//! channel publishes one bounded sink, the gateway forwards exact request
//! bytes to it, and a one-shot carries only the outcome needed for the public
//! HTTP response.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{mpsc, oneshot, watch};

/// Cancellation signal for one gateway-owned plugin webhook request.
pub type WebhookCancellation = tokio_util::sync::CancellationToken;

/// The state shared with a duplicate while the first delivery owns a stable
/// message ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookReservationStatus {
    /// The owning request has not completed delivery yet.
    InFlight,
    /// The owning request delivered the message successfully.
    Committed,
    /// The owning request failed before delivery and released the ID.
    RolledBack,
}

/// Opaque ownership proof for one in-flight idempotency reservation.
///
/// The generation prevents a delayed owner from committing or rolling back a
/// later request that acquired the same key after the first owner failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookReservationToken {
    key: String,
    generation: u64,
}

impl WebhookReservationToken {
    /// Create a token at the gateway-owned idempotency boundary.
    #[must_use]
    pub fn new(key: String, generation: u64) -> Self {
        Self { key, generation }
    }

    /// Storage key owned by the gateway idempotency store.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Monotonic ownership generation for this storage key.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Wait handle returned to a request that meets an in-flight owner.
pub struct WebhookReservationWaiter {
    status: watch::Receiver<WebhookReservationStatus>,
}

impl WebhookReservationWaiter {
    /// Wrap the gateway store's reservation watch channel.
    #[must_use]
    pub fn new(status: watch::Receiver<WebhookReservationStatus>) -> Self {
        Self { status }
    }

    /// Wait until the owner commits or rolls back.
    ///
    /// A dropped sender is treated as rollback: no successful owner remains to
    /// justify acknowledging the duplicate.
    pub async fn wait(&mut self) -> WebhookReservationStatus {
        loop {
            let status = *self.status.borrow_and_update();
            if status != WebhookReservationStatus::InFlight {
                return status;
            }
            if self.status.changed().await.is_err() {
                return WebhookReservationStatus::RolledBack;
            }
        }
    }
}

/// Result of trying to reserve one authenticated, host-authorized message ID.
pub enum WebhookReservation {
    /// This request owns delivery and must commit or roll back with the token.
    Owner(WebhookReservationToken),
    /// A prior owner already delivered the message.
    Committed,
    /// Another request owns delivery; wait for its actual outcome.
    InFlight(WebhookReservationWaiter),
    /// The bounded gateway store cannot safely admit another owner.
    Unavailable,
}

/// Live callbacks into the gateway's canonical idempotency store.
///
/// The plugin worker invokes this only after guest authentication and live
/// host sender authorization. Ownership tokens make rollback conditional, and
/// an in-flight duplicate receives a waiter rather than a premature success.
#[derive(Clone)]
pub struct WebhookIdempotency {
    begin: Arc<dyn Fn(&str) -> WebhookReservation + Send + Sync>,
    commit: Arc<dyn Fn(&WebhookReservationToken) -> bool + Send + Sync>,
    rollback: Arc<dyn Fn(&WebhookReservationToken) -> bool + Send + Sync>,
}

impl WebhookIdempotency {
    /// Build a bridge to a host-owned reservation store.
    #[must_use]
    pub fn new(
        begin: impl Fn(&str) -> WebhookReservation + Send + Sync + 'static,
        commit: impl Fn(&WebhookReservationToken) -> bool + Send + Sync + 'static,
        rollback: impl Fn(&WebhookReservationToken) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            begin: Arc::new(begin),
            commit: Arc::new(commit),
            rollback: Arc::new(rollback),
        }
    }

    /// Begin or observe delivery for one stable message ID.
    #[must_use]
    pub fn begin(&self, message_id: &str) -> WebhookReservation {
        (self.begin)(message_id)
    }

    /// Commit only if `token` still owns the exact reservation generation.
    #[must_use]
    pub fn commit(&self, token: &WebhookReservationToken) -> bool {
        (self.commit)(token)
    }

    /// Roll back only if `token` still owns the exact reservation generation.
    #[must_use]
    pub fn rollback(&self, token: &WebhookReservationToken) -> bool {
        (self.rollback)(token)
    }
}

/// A raw inbound request received on `/plugin/{path}`.
pub struct RawWebhook {
    /// HTTP method supplied by the gateway request, never by a header.
    pub method: String,
    /// Raw query string, without the leading `?`.
    pub query: String,
    /// Header names normalized to lowercase and their UTF-8 values.
    pub headers: Vec<(String, String)>,
    /// Exact received body bytes.
    pub body: Vec<u8>,
    /// Request-lifetime cancellation shared with the disposable guest parser.
    pub cancellation: WebhookCancellation,
    /// Access to the gateway's canonical idempotency store.
    pub idempotency: Option<WebhookIdempotency>,
    /// Outcome used to select the public HTTP response.
    pub reply: oneshot::Sender<Result<WebhookOutcome, WebhookReject>>,
}

/// Maximum UTF-8 byte length of a plugin's HTTP challenge response.
pub const MAX_WEBHOOK_RESPONSE_BODY_BYTES: usize = 4 * 1024;

/// Acknowledgement after message delivery, or a response with no agent work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookOutcome {
    Ack,
    Body(String),
}

/// Why a plugin webhook request could not be accepted.
#[derive(Debug, Clone)]
pub enum WebhookReject {
    /// Guest authentication failed. The detail is for host logs only.
    Unauthorized(String),
    /// The authenticated payload was malformed. Detail is host-only.
    BadRequest(String),
    /// Guest execution or downstream delivery was unavailable. Detail is
    /// host-only; the public response is always an opaque 503.
    Unavailable(String),
    /// The plugin's response exceeds the host-owned body limit (opaque 502).
    InvalidResponse,
    /// The request deadline cancelled guest parsing or delivery.
    Timeout,
}

/// Path-to-sink view shared by the gateway and channel supervisor.
///
/// The map is replaced as one unit after every claimant has been validated, so
/// a partial rebuild or duplicate path cannot leave a mixed-generation route
/// set effective. Lookups return a [`PluginWebhookRoute`], never a sink, so
/// every send uses the generation that is live when it happens.
#[derive(Default, Clone)]
pub struct PluginWebhookRegistry {
    state: Arc<Mutex<PluginWebhookRegistryState>>,
}

#[derive(Default)]
struct PluginWebhookRegistryState {
    generation: u64,
    routes: HashMap<String, mpsc::Sender<RawWebhook>>,
}

/// Ownership proof for one channel-supervisor route generation.
///
/// Publishing and retirement are conditional on this generation still being
/// current, so a delayed older supervisor cannot erase newer routes.
pub struct PluginWebhookRegistryLease {
    registry: PluginWebhookRegistry,
    generation: u64,
}

/// A path a live channel owned when the gateway looked it up.
///
/// The route names the path, not its sink. Every send resolves the owner under
/// the registry lock, so a generation that starts between lookup and send is
/// honored: the request reaches the new owner of a re-registered path and is
/// refused when the path is no longer published. It is never queued on a
/// retiring channel's receiver.
#[derive(Clone)]
pub struct PluginWebhookRoute {
    registry: PluginWebhookRegistry,
    path: String,
}

/// Why a [`PluginWebhookRoute`] could not queue a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginWebhookSendError {
    /// The current owner's bounded queue is at capacity.
    Full,
    /// No current owner can take the request: its receiver is gone, or the
    /// current generation has not published this path.
    Closed,
}

impl PluginWebhookRegistry {
    /// Create an empty runtime routing view.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a new empty route generation owned by the returned lease.
    #[must_use]
    pub fn start_generation(&self) -> PluginWebhookRegistryLease {
        let mut state = self.lock_state();
        state.generation = state.generation.wrapping_add(1);
        state.routes.clear();
        PluginWebhookRegistryLease {
            registry: self.clone(),
            generation: state.generation,
        }
    }

    /// Resolve `path` to a route, if a live channel owns it.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<PluginWebhookRoute> {
        let owned = self.lock_state().routes.contains_key(path);
        owned.then(|| PluginWebhookRoute {
            registry: self.clone(),
            path: path.to_string(),
        })
    }

    fn lock_state(&self) -> MutexGuard<'_, PluginWebhookRegistryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PluginWebhookRegistryLease {
    /// Atomically publish all validated routes if this generation still owns
    /// the registry. Returns `false` when a newer generation already started.
    #[must_use]
    pub fn replace(&self, routes: HashMap<String, mpsc::Sender<RawWebhook>>) -> bool {
        let mut state = self.registry.lock_state();
        if state.generation != self.generation {
            return false;
        }
        state.routes = routes;
        true
    }
}

impl PluginWebhookRoute {
    /// Queue `request` on the path's current owner without waiting. A refused
    /// request is dropped, which drops its reply sender.
    pub fn try_send(&self, request: RawWebhook) -> Result<(), PluginWebhookSendError> {
        // Resolve and enqueue under one guard: a generation cannot start or
        // publish between choosing the owner and queuing on it.
        let state = self.registry.lock_state();
        let Some(sink) = state.routes.get(&self.path) else {
            return Err(PluginWebhookSendError::Closed);
        };
        sink.try_send(request).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => PluginWebhookSendError::Full,
            mpsc::error::TrySendError::Closed(_) => PluginWebhookSendError::Closed,
        })
    }
}

impl Drop for PluginWebhookRegistryLease {
    fn drop(&mut self) {
        let mut state = self.registry.lock_state();
        if state.generation == self.generation {
            state.routes.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PluginWebhookRegistry, RawWebhook, WebhookOutcome, WebhookReject};

    fn raw_webhook(
        body: &[u8],
    ) -> (
        RawWebhook,
        tokio::sync::oneshot::Receiver<Result<WebhookOutcome, WebhookReject>>,
    ) {
        let (reply, outcome) = tokio::sync::oneshot::channel();
        let request = RawWebhook {
            method: "POST".to_string(),
            query: String::new(),
            headers: Vec::new(),
            body: body.to_vec(),
            cancellation: super::WebhookCancellation::new(),
            idempotency: None,
            reply,
        };
        (request, outcome)
    }

    #[test]
    fn a_route_resolved_before_a_generation_change_never_reaches_the_retired_owner() {
        let registry = PluginWebhookRegistry::new();
        let retiring = registry.start_generation();
        let (retired_sink, mut retired_rx) = tokio::sync::mpsc::channel(4);
        assert!(retiring.replace(std::collections::HashMap::from([(
            "gone".to_string(),
            retired_sink,
        )])));
        let route = registry
            .get("gone")
            .expect("the retiring generation owns the path");

        // The retiring supervisor still holds its receiver open while the next
        // generation builds, then publishes a route set without this path.
        let current = registry.start_generation();
        let (while_building, _outcome) = raw_webhook(b"while-building");
        assert!(
            route.try_send(while_building).is_err(),
            "a path the building generation has not published refuses the request"
        );
        assert!(current.replace(std::collections::HashMap::new()));
        let (after_publish, _outcome) = raw_webhook(b"after-publish");
        assert!(
            route.try_send(after_publish).is_err(),
            "a path the new generation withdrew refuses the request"
        );

        assert!(
            retired_rx.try_recv().is_err(),
            "a retired generation's receiver must never be handed a request"
        );
        drop(retiring);
    }

    #[test]
    fn a_route_resolved_before_re_registration_reaches_the_new_owner() {
        let registry = PluginWebhookRegistry::new();
        let retiring = registry.start_generation();
        let (retired_sink, mut retired_rx) = tokio::sync::mpsc::channel(4);
        assert!(retiring.replace(std::collections::HashMap::from([(
            "same".to_string(),
            retired_sink,
        )])));
        let route = registry
            .get("same")
            .expect("the retiring generation owns the path");

        let current = registry.start_generation();
        let (current_sink, mut current_rx) = tokio::sync::mpsc::channel(4);
        assert!(current.replace(std::collections::HashMap::from([(
            "same".to_string(),
            current_sink,
        )])));

        let (request, _outcome) = raw_webhook(b"re-registered");
        route
            .try_send(request)
            .expect("an unchanged path re-registered by the new generation still dispatches");
        let delivered = current_rx
            .try_recv()
            .expect("the new generation's owner receives the request");
        assert_eq!(delivered.body, b"re-registered");
        assert!(
            retired_rx.try_recv().is_err(),
            "a retired generation's receiver must never be handed a request"
        );
        drop(retiring);
    }

    #[test]
    fn route_sends_tell_a_full_owner_apart_from_a_missing_one() {
        use super::PluginWebhookSendError;

        let registry = PluginWebhookRegistry::new();
        let lease = registry.start_generation();
        let (sink, receiver) = tokio::sync::mpsc::channel(1);
        assert!(lease.replace(std::collections::HashMap::from([(
            "busy".to_string(),
            sink,
        )])));
        let route = registry.get("busy").expect("the generation owns the path");

        assert_eq!(route.try_send(raw_webhook(b"first").0), Ok(()));
        assert_eq!(
            route.try_send(raw_webhook(b"second").0),
            Err(PluginWebhookSendError::Full)
        );
        drop(receiver);
        assert_eq!(
            route.try_send(raw_webhook(b"third").0),
            Err(PluginWebhookSendError::Closed)
        );
        let _next = registry.start_generation();
        assert_eq!(
            route.try_send(raw_webhook(b"fourth").0),
            Err(PluginWebhookSendError::Closed)
        );
    }

    #[test]
    fn registry_recovers_after_a_poisoned_lock() {
        let registry = PluginWebhookRegistry::new();
        let poison_target = registry.clone();
        let poisoner = std::thread::spawn(move || {
            let state = poison_target
                .state
                .lock()
                .expect("test obtains registry lock");
            assert!(state.routes.is_empty());
            panic!("poison registry for recovery test");
        });
        assert!(poisoner.join().is_err());

        let (sink, receiver) = tokio::sync::mpsc::channel(1);
        let lease = registry.start_generation();
        assert!(lease.replace(std::collections::HashMap::from([(
            "fixture".to_string(),
            sink
        )])));
        assert!(registry.get("fixture").is_some());
        drop(receiver);
    }

    #[test]
    fn replace_publishes_one_complete_generation() {
        let registry = PluginWebhookRegistry::new();
        let (old, old_rx) = tokio::sync::mpsc::channel(1);
        let lease = registry.start_generation();
        assert!(lease.replace(std::collections::HashMap::from([("old".to_string(), old)])));

        let (new, new_rx) = tokio::sync::mpsc::channel(1);
        assert!(lease.replace(std::collections::HashMap::from([("new".to_string(), new)])));

        assert!(registry.get("old").is_none());
        assert!(registry.get("new").is_some());
        drop((old_rx, new_rx));
    }

    #[test]
    fn stale_generation_cannot_publish_or_clear_newer_routes() {
        let registry = PluginWebhookRegistry::new();
        let stale = registry.start_generation();
        let (stale_sink, stale_rx) = tokio::sync::mpsc::channel(1);
        assert!(stale.replace(std::collections::HashMap::from([(
            "stale".to_string(),
            stale_sink,
        )])));

        let current = registry.start_generation();
        let (current_sink, current_rx) = tokio::sync::mpsc::channel(1);
        assert!(current.replace(std::collections::HashMap::from([(
            "current".to_string(),
            current_sink,
        )])));
        let (late_sink, late_rx) = tokio::sync::mpsc::channel(1);
        assert!(!stale.replace(std::collections::HashMap::from([(
            "late".to_string(),
            late_sink,
        )])));
        drop(stale);

        assert!(registry.get("stale").is_none());
        assert!(registry.get("late").is_none());
        assert!(registry.get("current").is_some());
        drop(current);
        assert!(registry.get("current").is_none());
        drop((stale_rx, current_rx, late_rx));
    }
}
