//! Per-session actor queue for serializing concurrent access.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// Per-session serialization queue.
pub struct SessionActorQueue {
    slots: Mutex<HashMap<String, Arc<SessionSlot>>>,
    max_queue_depth: usize,
    lock_timeout: Duration,
    idle_ttl: Duration,
}

struct SessionSlot {
    semaphore: Arc<Semaphore>,
    last_active: Mutex<Instant>,
    pending: AtomicUsize,
}

/// RAII guard that releases the session permit and pending count on drop.
///
/// Drop order is significant: the semaphore permit is released *before*
/// `pending` is decremented, so a racing `evict_idle` always observes a
/// consistent state — `pending > 0` (permit still held) or `pending == 0`
/// (permit already released). Implemented with `ManuallyDrop` so `Drop`
/// releases the permit explicitly before touching `pending`.
pub struct SessionGuard {
    slot: Arc<SessionSlot>,
    _permit: std::mem::ManuallyDrop<OwnedSemaphorePermit>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        // Release the semaphore permit FIRST, then decrement
        // `pending`. This closes the race with `evict_idle`:
        // without this ordering, `evict_idle` could see
        // `pending == 0` (just decremented) while the permit
        // was still held, and evict the slot from the map
        // under the permit.
        //
        // SAFETY: `_permit` is only ever consumed here in
        // `Drop::drop`. The `ManuallyDrop` wrapper suppresses
        // the inner `Drop`, so the permit is dropped exactly
        // once. After `take`, the `ManuallyDrop` cell is
        // uninitialised but is itself a no-op to drop, so the
        // implicit field drop that follows this function is
        // safe.
        let permit = unsafe { std::mem::ManuallyDrop::take(&mut self._permit) };
        drop(permit);
        self.slot.pending.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Cancel-safe guard for the pending-count increment during `acquire`.
///
/// On successful acquisition, `into_guard()` transfers the +1 to
/// `SessionGuard`. On error or cancellation, `Drop` decrements.
struct PendingGuard {
    slot: Arc<SessionSlot>,
    consumed: bool,
}

impl PendingGuard {
    fn into_guard(mut self) {
        self.consumed = true;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if !self.consumed {
            self.slot.pending.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Errors from the session queue.
#[derive(Debug)]
pub enum SessionQueueError {
    /// Too many requests queued for this session.
    QueueFull { session_id: String, depth: usize },
    /// Timed out waiting for the session lock.
    Timeout { session_id: String },
}

impl std::fmt::Display for SessionQueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull { session_id, depth } => {
                write!(
                    f,
                    "Session {session_id} queue full ({depth} pending requests)"
                )
            }
            Self::Timeout { session_id } => {
                write!(f, "Timed out waiting for session {session_id}")
            }
        }
    }
}

impl std::error::Error for SessionQueueError {}

impl SessionActorQueue {
    /// Create a new queue with the given limits.
    pub fn new(max_queue_depth: usize, lock_timeout_secs: u64, idle_ttl_secs: u64) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            max_queue_depth,
            lock_timeout: Duration::from_secs(lock_timeout_secs),
            idle_ttl: Duration::from_secs(idle_ttl_secs),
        }
    }

    /// Acquire exclusive access to a session. Cancel-safe: `PendingGuard`
    /// ensures the pending count is decremented even if this future is dropped.
    pub async fn acquire(&self, session_id: &str) -> Result<SessionGuard, SessionQueueError> {
        let (slot, current) = {
            let mut slots = self.slots.lock().await;
            let s = slots
                // Key slots by the raw, case-sensitive session key, matching
                // how ownership and transcript rows are stored. Folding case
                // would serialize distinct canonical sessions onto one slot.
                .entry(session_id.to_string())
                .or_insert_with(|| {
                    Arc::new(SessionSlot {
                        semaphore: Arc::new(Semaphore::new(1)),
                        last_active: Mutex::new(Instant::now()),
                        pending: AtomicUsize::new(0),
                    })
                })
                .clone();
            let current = s.pending.fetch_add(1, Ordering::Relaxed);
            (s, current)
        };
        let pending_guard = PendingGuard {
            slot: slot.clone(),
            consumed: false,
        };
        if current >= self.max_queue_depth {
            return Err(SessionQueueError::QueueFull {
                session_id: session_id.to_string(),
                depth: current,
            });
        }

        let sem = slot.semaphore.clone();
        match tokio::time::timeout(self.lock_timeout, sem.acquire_owned()).await {
            Ok(Ok(permit)) => {
                *slot.last_active.lock().await = Instant::now();
                pending_guard.into_guard();
                Ok(SessionGuard {
                    slot,
                    _permit: std::mem::ManuallyDrop::new(permit),
                })
            }
            Ok(Err(_)) | Err(_) => Err(SessionQueueError::Timeout {
                session_id: session_id.to_string(),
            }),
        }
    }

    /// Get the number of pending requests for a session.
    pub async fn queue_depth(&self, session_id: &str) -> usize {
        let guard_key = session_id.to_string();
        let slots = self.slots.lock().await;
        slots
            .get(&guard_key)
            .map(|s| s.pending.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Remove idle session slots that haven't been accessed within the TTL.
    pub async fn evict_idle(&self) -> usize {
        let mut slots = self.slots.lock().await;
        let now = Instant::now();
        let before = slots.len();
        let ttl = self.idle_ttl;

        let mut to_remove = Vec::new();
        for (key, slot) in slots.iter() {
            let last = *slot.last_active.lock().await;
            if now.duration_since(last) > ttl && slot.pending.load(Ordering::Relaxed) == 0 {
                to_remove.push(key.clone());
            }
        }
        for key in &to_remove {
            slots.remove(key);
        }

        before - slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serializes_same_session() {
        let queue = SessionActorQueue::new(8, 5, 600);

        // Acquire and release, then re-acquire should work
        let guard1 = queue.acquire("s1").await.unwrap();
        drop(guard1);
        let _guard2 = queue.acquire("s1").await.unwrap();
    }

    #[tokio::test]
    async fn parallel_different_sessions() {
        let queue = SessionActorQueue::new(8, 5, 600);
        let _guard1 = queue.acquire("s1").await.unwrap();
        let _guard2 = queue.acquire("s2").await.unwrap();
        // Both acquired simultaneously — different sessions don't block each other
    }

    #[tokio::test]
    async fn queue_depth_limit() {
        let queue = Arc::new(SessionActorQueue::new(2, 30, 600));

        // Hold the session lock (pending=1)
        let guard = queue.acquire("s1").await.unwrap();

        // Queue one more (pending=2, will block waiting for permit)
        let queue_clone = queue.clone();
        let handle = zeroclaw_spawn::spawn!(async move { queue_clone.acquire("s1").await });

        // Give the spawned task time to register
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Third request should be rejected (pending=2 >= max=2)
        let result = queue.acquire("s1").await;
        assert!(matches!(result, Err(SessionQueueError::QueueFull { .. })));

        drop(guard);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn timeout_returns_error() {
        let queue = SessionActorQueue::new(8, 1, 600);
        let _guard = queue.acquire("s1").await.unwrap();

        let start = Instant::now();
        let result = queue.acquire("s1").await;
        assert!(matches!(result, Err(SessionQueueError::Timeout { .. })));
        assert!(start.elapsed() >= Duration::from_millis(900));
    }

    #[tokio::test]
    async fn idle_eviction() {
        let queue = SessionActorQueue::new(8, 5, 0); // 0s TTL
        {
            let _guard = queue.acquire("s1").await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        let evicted = queue.evict_idle().await;
        assert_eq!(evicted, 1);
    }

    /// While a `SessionGuard` is alive, the slot must remain in the map
    /// even past the idle TTL: releasing the permit *before* decrementing
    /// `pending` means `evict_idle` never observes a half-released slot —
    /// `pending > 0` ⇒ permit still held (cannot evict), `pending == 0` ⇒
    /// permit already released.
    #[tokio::test]
    async fn evict_idle_keeps_held_permit_slot() {
        let queue = SessionActorQueue::new(8, 5, 0); // 0s TTL
        let _guard = queue.acquire("s1").await.unwrap();
        // Sleep well past the TTL. last_active is set on acquire,
        // but TTL=0 means anything older than "now" is idle —
        // except for the held permit, which `pending > 0`
        // must protect.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let evicted = queue.evict_idle().await;
        assert_eq!(
            evicted, 0,
            "a slot whose permit is still held (pending > 0) must not be evicted"
        );
    }

    /// After the guard drops, the slot is evictable on the next
    /// `evict_idle`: the permit is released and `pending == 0`.
    #[tokio::test]
    async fn evict_idle_collects_after_guard_drop() {
        let queue = SessionActorQueue::new(8, 5, 0);
        {
            let _guard = queue.acquire("s1").await.unwrap();
        }
        // The guard is fully dropped (permit released + pending=0)
        // by the time we sleep.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let evicted = queue.evict_idle().await;
        assert_eq!(evicted, 1, "after drop, the idle slot must be evicted");
    }

    #[tokio::test]
    async fn queue_depth_reports_correctly() {
        let queue = SessionActorQueue::new(8, 30, 600);
        assert_eq!(queue.queue_depth("s1").await, 0);

        let guard = queue.acquire("s1").await.unwrap();
        assert_eq!(queue.queue_depth("s1").await, 1);

        drop(guard);
        assert_eq!(queue.queue_depth("s1").await, 0);
    }

    /// Deterministic FIFO interleaving: once several waiters have registered,
    /// releasing the holder lets them drain one at a time in acquisition order.
    /// Registration is detected by polling `queue_depth` up to a bounded spin,
    /// never by sleeping on a wall clock.
    #[tokio::test]
    async fn waiters_drain_in_order_after_release() {
        let queue = Arc::new(SessionActorQueue::new(4, 30, 600));
        let guard0 = queue.acquire("s1").await.unwrap();

        let mut tasks = Vec::new();
        for _ in 0..2 {
            let q = queue.clone();
            tasks.push(zeroclaw_spawn::spawn!(async move { q.acquire("s1").await }));
        }

        // Bound the spin instead of sleeping: keep yielding until both
        // waiters have registered (guard + 2 waiters = depth 3).
        for _ in 0..1000 {
            if queue.queue_depth("s1").await == 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            queue.queue_depth("s1").await,
            3,
            "both waiters must have registered before the holder is released"
        );

        // Releasing the holder lets the queued waiters acquire in order;
        // each join resolves only after that waiter has the permit.
        drop(guard0);
        for t in tasks {
            let _guard = t.await.unwrap().unwrap();
        }
        assert_eq!(queue.queue_depth("s1").await, 0);
    }

    /// Cancellation safety: a waiter aborted while parked on the semaphore
    /// must release its pending count (the `PendingGuard` RAII path), so it
    /// can never skew the depth accounting or block a later `evict_idle`.
    /// The task is joined before asserting, so the drop has completed.
    #[tokio::test]
    async fn cancelled_waiter_releases_pending_count() {
        let queue = Arc::new(SessionActorQueue::new(8, 30, 600));
        let guard0 = queue.acquire("s1").await.unwrap();

        let q = queue.clone();
        let task = zeroclaw_spawn::spawn!(async move { q.acquire("s1").await });
        for _ in 0..1000 {
            if queue.queue_depth("s1").await == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(queue.queue_depth("s1").await, 2, "waiter must register");

        task.abort();
        let _ = task.await; // join resolves only once the task (and its PendingGuard) has dropped
        assert_eq!(
            queue.queue_depth("s1").await,
            1,
            "aborted waiter must release its pending count"
        );

        drop(guard0);
        assert_eq!(queue.queue_depth("s1").await, 0);
    }
}
