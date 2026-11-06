//! Wasmtime-free wiring for channel-plugin webhook ingress.
//!
//! The gateway and channel supervisor deliberately meet in this API crate so
//! the HTTP boundary does not need to depend on the plugin runtime. A running
//! channel publishes one bounded sink, the gateway forwards exact request
//! bytes to it, and a one-shot carries only the outcome needed for the public
//! HTTP response.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

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

/// The deduplication store behind every webhook ingress of one gateway run.
///
/// A plugin delivery reserves its message's key, then commits it once the
/// message is enqueued for the channel or rolls it back when the delivery
/// fails. A duplicate of a committed key is acknowledged without being
/// delivered again; a duplicate of a key still in flight waits for its
/// owner's outcome. The generic `/webhook` and `/sop/*` routes record their
/// keys through [`Self::record_if_new`].
///
/// Both kinds share one committed map bounded by `max_keys`, so committing
/// either kind evicts the oldest committed key of either kind. Committed keys
/// expire after `ttl`. In-flight plugin reservations are bounded separately by
/// the same `max_keys`, and a new delivery is refused while every in-flight
/// slot is taken.
///
/// The daemon creates one store for each gateway run, so its contents last
/// exactly as long as that run.
#[derive(Debug)]
pub struct WebhookReservationStore {
    ttl: Duration,
    max_keys: usize,
    entries: Mutex<ReservationEntries>,
    next_generation: AtomicU64,
}

#[derive(Debug, Default)]
struct ReservationEntries {
    committed: HashMap<String, Instant>,
    pending: HashMap<String, PendingReservation>,
}

#[derive(Debug)]
struct PendingReservation {
    generation: u64,
    status: watch::Sender<WebhookReservationStatus>,
}

impl WebhookReservationStore {
    /// An empty store keeping committed keys for `ttl`, with at most
    /// `max_keys` committed and at most `max_keys` in-flight keys.
    #[must_use]
    pub fn new(ttl: Duration, max_keys: usize) -> Self {
        Self {
            ttl,
            max_keys: max_keys.max(1),
            entries: Mutex::new(ReservationEntries::default()),
            next_generation: AtomicU64::new(1),
        }
    }

    fn entries(&self) -> MutexGuard<'_, ReservationEntries> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reserve `key` for a new delivery, or report the state of the delivery
    /// that already holds it.
    #[must_use]
    pub fn begin(&self, key: &str) -> WebhookReservation {
        let now = Instant::now();
        let mut entries = self.entries();
        entries
            .committed
            .retain(|_, seen_at| now.duration_since(*seen_at) < self.ttl);
        if entries.committed.contains_key(key) {
            return WebhookReservation::Committed;
        }
        if let Some(pending) = entries.pending.get(key) {
            return WebhookReservation::InFlight(WebhookReservationWaiter::new(
                pending.status.subscribe(),
            ));
        }
        if entries.pending.len() >= self.max_keys {
            return WebhookReservation::Unavailable;
        }
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let (status, _) = watch::channel(WebhookReservationStatus::InFlight);
        entries
            .pending
            .insert(key.to_string(), PendingReservation { generation, status });
        WebhookReservation::Owner(WebhookReservationToken::new(key.to_string(), generation))
    }

    /// Commit the reservation `token` owns. `false` when it no longer owns
    /// one (already settled, or a later reservation took the key).
    #[must_use]
    pub fn commit(&self, token: &WebhookReservationToken) -> bool {
        let mut entries = self.entries();
        let Some(pending) = Self::take_owned(&mut entries, token) else {
            return false;
        };
        pending
            .status
            .send_replace(WebhookReservationStatus::Committed);
        let now = Instant::now();
        entries
            .committed
            .retain(|_, seen_at| now.duration_since(*seen_at) < self.ttl);
        Self::make_committed_room(&mut entries, self.max_keys);
        entries.committed.insert(token.key().to_string(), now);
        true
    }

    /// Record `key` for a generic webhook request. `true` when the key is new
    /// and is now recorded; `false` for a key already committed or held by an
    /// in-flight plugin delivery. Records count against the same committed
    /// bound as plugin deliveries.
    #[must_use]
    pub fn record_if_new(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut entries = self.entries();
        entries
            .committed
            .retain(|_, seen_at| now.duration_since(*seen_at) < self.ttl);
        if entries.committed.contains_key(key) || entries.pending.contains_key(key) {
            return false;
        }
        Self::make_committed_room(&mut entries, self.max_keys);
        entries.committed.insert(key.to_owned(), now);
        true
    }

    /// Committed keys currently held, across both ingress kinds. Expired keys
    /// are dropped on the next record or reservation, not by this call.
    #[must_use]
    pub fn committed_len(&self) -> usize {
        self.entries().committed.len()
    }

    /// Evict the oldest committed key when the committed map is full.
    fn make_committed_room(entries: &mut ReservationEntries, max_keys: usize) {
        if entries.committed.len() < max_keys {
            return;
        }
        let oldest = entries
            .committed
            .iter()
            .min_by_key(|(_, seen_at)| *seen_at)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            entries.committed.remove(&oldest);
        }
    }

    /// Release the reservation `token` owns, so a retry can deliver. `false`
    /// when it no longer owns one.
    #[must_use]
    pub fn rollback(&self, token: &WebhookReservationToken) -> bool {
        let mut entries = self.entries();
        let Some(pending) = Self::take_owned(&mut entries, token) else {
            return false;
        };
        pending
            .status
            .send_replace(WebhookReservationStatus::RolledBack);
        true
    }

    /// Remove and return the pending reservation `token` owns, if it still
    /// owns that exact generation.
    fn take_owned(
        entries: &mut ReservationEntries,
        token: &WebhookReservationToken,
    ) -> Option<PendingReservation> {
        if entries
            .pending
            .get(token.key())
            .is_none_or(|pending| pending.generation != token.generation())
        {
            return None;
        }
        entries.pending.remove(token.key())
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
/// set effective.
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

    /// Clone the bounded sink for `path`, if a live channel owns it.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<mpsc::Sender<RawWebhook>> {
        self.lock_state().routes.get(path).cloned()
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
    use super::PluginWebhookRegistry;

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

    #[tokio::test]
    async fn reservations_wait_for_the_owners_outcome_and_fence_stale_tokens() {
        use super::{WebhookReservation, WebhookReservationStatus, WebhookReservationStore};
        use std::time::Duration;

        let store = WebhookReservationStore::new(Duration::from_secs(300), 8);
        let first = match store.begin("stable-id") {
            WebhookReservation::Owner(token) => token,
            _ => panic!("first request must own the reservation"),
        };
        let mut duplicate = match store.begin("stable-id") {
            WebhookReservation::InFlight(waiter) => waiter,
            _ => panic!("duplicate must observe an in-flight owner"),
        };

        assert!(store.rollback(&first));
        assert_eq!(duplicate.wait().await, WebhookReservationStatus::RolledBack);
        let replacement = match store.begin("stable-id") {
            WebhookReservation::Owner(token) => token,
            _ => panic!("duplicate must acquire after owner rollback"),
        };
        assert_ne!(first.generation(), replacement.generation());
        assert!(!store.rollback(&first));

        let mut committed_duplicate = match store.begin("stable-id") {
            WebhookReservation::InFlight(waiter) => waiter,
            _ => panic!("later duplicate must wait for replacement owner"),
        };
        assert!(store.commit(&replacement));
        assert_eq!(
            committed_duplicate.wait().await,
            WebhookReservationStatus::Committed
        );
        assert!(matches!(
            store.begin("stable-id"),
            WebhookReservation::Committed
        ));
    }

    #[test]
    fn in_flight_reservations_are_bounded_and_released_by_rollback() {
        use super::{WebhookReservation, WebhookReservationStore};
        use std::time::Duration;

        let store = WebhookReservationStore::new(Duration::from_secs(300), 1);
        let owner = match store.begin("first") {
            WebhookReservation::Owner(token) => token,
            _ => panic!("first plugin delivery owns the pending slot"),
        };
        assert!(matches!(
            store.begin("second"),
            WebhookReservation::Unavailable
        ));
        assert!(store.rollback(&owner));

        let replacement = match store.begin("second") {
            WebhookReservation::Owner(token) => token,
            _ => panic!("rolling back frees the bounded pending slot"),
        };
        assert!(store.commit(&replacement));
        let entries = store.entries();
        assert_eq!(entries.pending.len(), 0);
        assert_eq!(entries.committed.len(), 1);
        assert!(entries.committed.contains_key(replacement.key()));
    }

    #[test]
    fn committed_keys_expire_and_the_oldest_is_evicted_at_capacity() {
        use super::{WebhookReservation, WebhookReservationStore};
        use std::time::Duration;

        let commit = |store: &WebhookReservationStore, key: &str| match store.begin(key) {
            WebhookReservation::Owner(token) => assert!(store.commit(&token)),
            _ => panic!("{key} must be new"),
        };

        let bounded = WebhookReservationStore::new(Duration::from_secs(300), 2);
        commit(&bounded, "a");
        std::thread::sleep(Duration::from_millis(5));
        commit(&bounded, "b");
        std::thread::sleep(Duration::from_millis(5));
        commit(&bounded, "c");
        assert!(
            matches!(bounded.begin("a"), WebhookReservation::Owner(_)),
            "the oldest committed key was evicted to make room"
        );
        assert!(matches!(bounded.begin("c"), WebhookReservation::Committed));

        let expiring = WebhookReservationStore::new(Duration::from_millis(1), 8);
        commit(&expiring, "short");
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            matches!(expiring.begin("short"), WebhookReservation::Owner(_)),
            "a committed key past its TTL is no longer a duplicate"
        );
    }

    #[test]
    fn generic_records_and_plugin_commits_share_one_committed_budget() {
        use super::{WebhookReservation, WebhookReservationStore};
        use std::time::Duration;

        let commit = |store: &WebhookReservationStore, key: &str| match store.begin(key) {
            WebhookReservation::Owner(token) => assert!(store.commit(&token)),
            _ => panic!("{key} must be new"),
        };

        // A plugin commit, then a generic record, with room for one key: the
        // generic record evicts the plugin key, so the plugin key is new again.
        let store = WebhookReservationStore::new(Duration::from_secs(300), 1);
        commit(&store, "plugin");
        assert!(store.record_if_new("generic"));
        assert_eq!(store.committed_len(), 1);
        assert!(matches!(
            store.begin("plugin"),
            WebhookReservation::Owner(_)
        ));

        // The other order: the plugin commit evicts the generic key.
        let store = WebhookReservationStore::new(Duration::from_secs(300), 1);
        assert!(store.record_if_new("generic"));
        commit(&store, "plugin");
        assert!(store.record_if_new("generic"));
    }

    #[test]
    fn a_generic_record_treats_an_in_flight_plugin_key_as_seen() {
        use super::{WebhookReservation, WebhookReservationStore};
        use std::time::Duration;

        let store = WebhookReservationStore::new(Duration::from_secs(300), 4);
        let owner = match store.begin("shared-key") {
            WebhookReservation::Owner(token) => token,
            _ => panic!("first delivery owns the key"),
        };
        assert!(!store.record_if_new("shared-key"));
        assert!(store.rollback(&owner));
        assert!(store.record_if_new("shared-key"));
        assert!(!store.record_if_new("shared-key"));
    }
}
