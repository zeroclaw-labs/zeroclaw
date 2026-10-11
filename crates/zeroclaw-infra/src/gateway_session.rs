//! Gateway session lifecycle coordination shared by the gateway and RPC.

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use crate::session_queue::SessionActorQueue;

const GATEWAY_QUEUE_DEPTH: usize = 8;
const GATEWAY_LOCK_TIMEOUT_SECS: u64 = 30;
const GATEWAY_IDLE_TTL_SECS: u64 = 600;

/// One daemon generation's gateway queue and turn-cancellation registry.
///
/// Supervised gateway restarts and RPC deletion share this handle. Standalone
/// gateways own a separate handle. RPC retains its intentionally different
/// depth-32 queue; only Chat deletion acquires both, in RPC-to-gateway order.
#[derive(Clone)]
pub struct GatewaySessionCoordination {
    queue: Arc<SessionActorQueue>,
    cancellations: Arc<Mutex<GatewayCancellationRegistry>>,
}

impl GatewaySessionCoordination {
    /// Construct the existing gateway depth-8, 30-second, 600-second policy.
    #[must_use]
    pub fn for_gateway() -> Self {
        Self {
            queue: Arc::new(SessionActorQueue::new(
                GATEWAY_QUEUE_DEPTH,
                GATEWAY_LOCK_TIMEOUT_SECS,
                GATEWAY_IDLE_TTL_SECS,
            )),
            cancellations: Arc::new(Mutex::new(GatewayCancellationRegistry::default())),
        }
    }

    /// The gateway's per-key serialization and generation authority.
    #[must_use]
    pub fn queue(&self) -> &Arc<SessionActorQueue> {
        &self.queue
    }

    /// The gateway's active-token and pending-deletion authority.
    #[must_use]
    pub fn cancellations(&self) -> &Arc<Mutex<GatewayCancellationRegistry>> {
        &self.cancellations
    }

    /// Signal one generation and retain its pending signal until completion.
    #[must_use]
    pub fn signal_deletion_at_generation(
        &self,
        session_key: &str,
        generation: u64,
    ) -> GatewayDeletionCancellation {
        signal_deletion_at_generation(&self.cancellations, session_key, generation)
    }
}

/// Gateway session key prefix to avoid collisions with channel sessions.
pub const GW_SESSION_PREFIX: &str = "gw_";

/// Return the canonical persistence key for a gateway session.
///
/// Persistence backends apply the shared filesystem-safe normalization so
/// their in-memory and on-disk keys remain consistent.
pub fn gateway_session_key(session_id: &str) -> String {
    format!(
        "{GW_SESSION_PREFIX}{}",
        zeroclaw_api::session_keys::sanitize_session_key(session_id)
    )
}

/// Return the process-local cancellation key for a gateway session.
///
/// Unlike persistence keys, cancellation keys must preserve the accepted
/// session id verbatim: filesystem-safe normalization is lossy and would make
/// distinct live sessions such as `team.alpha` and `team_alpha` cancel one
/// another.
pub fn gateway_cancel_key(session_id: &str) -> String {
    format!("{GW_SESSION_PREFIX}{session_id}")
}

/// Gateway turn-cancellation state guarded by one synchronous mutex.
///
/// The map remains the canonical active-turn lookup used by abort and status
/// handlers. Pending deletion signals exist only while a DELETE waits for the
/// same queue incarnation to finalize; they close the admission-registration
/// race without letting a stale delete affect a successor generation.
#[derive(Default)]
pub struct GatewayCancellationRegistry {
    tokens: HashMap<String, (u64, Arc<tokio_util::sync::CancellationToken>)>,
    pending_deletions: HashMap<String, HashSet<u64>>,
}

impl Deref for GatewayCancellationRegistry {
    type Target = HashMap<String, (u64, Arc<tokio_util::sync::CancellationToken>)>;

    fn deref(&self) -> &Self::Target {
        &self.tokens
    }
}

impl DerefMut for GatewayCancellationRegistry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tokens
    }
}

impl GatewayCancellationRegistry {
    /// Pending deletion generations, for lifecycle diagnostics and tests.
    pub fn pending_deletions(&self) -> &HashMap<String, HashSet<u64>> {
        &self.pending_deletions
    }
}

/// Register the current turn for a gateway session and cancel any replaced
/// turn before returning. Both HTTP/SSE and WebSocket transports share this
/// registry, so replacement ownership must be identical at both edges.
pub fn register_cancel_token(
    cancel_tokens: &Arc<std::sync::Mutex<GatewayCancellationRegistry>>,
    cancel_key: &str,
    session_key: &str,
    session_generation: u64,
    cancel_token: Arc<tokio_util::sync::CancellationToken>,
) {
    let (previous_token, pending_delete) = {
        let mut registry = cancel_tokens.lock().expect("cancel_tokens lock poisoned");
        let pending_delete = registry
            .pending_deletions
            .get(session_key)
            .is_some_and(|generations| generations.contains(&session_generation));
        let previous = registry.insert(
            cancel_key.to_owned(),
            (session_generation, Arc::clone(&cancel_token)),
        );
        (previous.map(|(_, token)| token), pending_delete)
    };
    if let Some(previous_token) = previous_token {
        previous_token.cancel();
    }
    if pending_delete {
        cancel_token.cancel();
    }
}

/// Remove a turn's registry entry only while it still owns the session key.
/// A late completion from a replaced WS/SSE turn must never remove the newer
/// turn's cancellation handle.
pub fn remove_cancel_token_if_current(
    cancel_tokens: &Arc<std::sync::Mutex<GatewayCancellationRegistry>>,
    cancel_key: &str,
    cancel_token: &Arc<tokio_util::sync::CancellationToken>,
) {
    let mut tokens = cancel_tokens.lock().expect("cancel_tokens lock poisoned");
    if tokens
        .get(cancel_key)
        .is_some_and(|(_, current)| Arc::ptr_eq(current, cancel_token))
    {
        tokens.remove(cancel_key);
    }
}

/// Lifecycle signal owned by one DELETE request.
///
/// An unconsumed pre-registration signal is removed when DELETE fails, so a
/// later turn cannot inherit a cancellation from a request that made no
/// durable lifecycle change.
pub struct GatewayDeletionCancellation {
    cancellations: Arc<Mutex<GatewayCancellationRegistry>>,
    session_key: String,
    session_generation: u64,
    pending: bool,
    /// Whether this signal cancelled an already registered turn.
    pub cancelled_active_turn: bool,
}

impl Drop for GatewayDeletionCancellation {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        let mut cancellations = self
            .cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(generations) = cancellations.pending_deletions.get_mut(&self.session_key) {
            generations.remove(&self.session_generation);
            if generations.is_empty() {
                cancellations.pending_deletions.remove(&self.session_key);
            }
        }
    }
}

/// Cancel an active gateway turn or atomically latch DELETE for an admitted
/// turn that has not registered its token yet.
///
/// Gateway session IDs are reusable after deletion. The cancellation registry
/// carries the same incarnation boundary as the session queue. The pending
/// latch and token registration share one mutex, so either DELETE observes and
/// cancels the exact active token or the later registration consumes the latch.
pub fn signal_deletion_at_generation(
    registry: &Arc<Mutex<GatewayCancellationRegistry>>,
    session_key: &str,
    expected_generation: u64,
) -> GatewayDeletionCancellation {
    let mut cancellations = registry.lock().unwrap_or_else(|error| error.into_inner());
    let mut cancelled_active_turn = false;
    for (cancel_key, (generation, token)) in cancellations.tokens.iter() {
        let canonical_key = gateway_session_key(
            cancel_key
                .strip_prefix(GW_SESSION_PREFIX)
                .unwrap_or(cancel_key),
        );
        // Preserve exact legacy dotted keys as well as the current sanitized
        // persistence key. Both may identify the raw active turn being reset.
        if (cancel_key == session_key || canonical_key == session_key)
            && *generation == expected_generation
        {
            token.cancel();
            cancelled_active_turn = true;
        }
    }
    if cancelled_active_turn {
        return GatewayDeletionCancellation {
            cancellations: Arc::clone(registry),
            session_key: session_key.to_owned(),
            session_generation: expected_generation,
            pending: false,
            cancelled_active_turn: true,
        };
    };
    // Retain each generation separately. A stale DELETE may arrive after a
    // successor DELETE has latched, and replacing the successor generation
    // would let the stale guard erase the successor's cancellation boundary.
    let pending = cancellations
        .pending_deletions
        .entry(session_key.to_string())
        .or_default()
        .insert(expected_generation);
    GatewayDeletionCancellation {
        cancellations: Arc::clone(registry),
        session_key: session_key.to_owned(),
        session_generation: expected_generation,
        pending,
        cancelled_active_turn: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn gateway_coordination_preserves_depth_eight() {
        let coordination = GatewaySessionCoordination::for_gateway();
        let key = "gw_depth";
        let held = coordination.queue().acquire(key).await.unwrap();
        let mut waiters = Vec::new();
        for _ in 1..8 {
            let queue = Arc::clone(coordination.queue());
            waiters.push(zeroclaw_spawn::spawn!(
                async move { queue.acquire(key).await }
            ));
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while coordination.queue().queue_depth(key).await != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            coordination.queue().acquire(key).await.is_err(),
            "ninth admission must be refused"
        );
        for waiter in waiters {
            waiter.abort();
            let error = waiter
                .await
                .err()
                .expect("aborted waiter must be cancelled");
            assert!(error.is_cancelled());
        }
        drop(held);
        assert_eq!(coordination.queue().queue_depth(key).await, 0);
    }
}
