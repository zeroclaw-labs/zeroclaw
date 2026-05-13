//! Per-key actor queue for serializing concurrent access.
//!
//! Each key (session id or slot id) gets at most one concurrent turn.
//! Additional requests queue up (bounded by `max_queue_depth`) and proceed in
//! FIFO order. This prevents SQLite history corruption from overlapping writes
//! and ensures consistent session state transitions.
//!
//! Two type aliases are exported over the same implementation:
//! - [`SessionActorQueue`] — legacy `/ws/chat` path, keyed on `session_id`.
//! - [`SlotActorQueue`] — dashboard path, keyed on `slot_id` (M1+).
//!
//! The name `slots` on the internal `HashMap` predates the dashboard
//! "slot" concept and refers to serialization slots in the queue.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// Per-session serialization queue (legacy `/ws/chat`, session-keyed).
pub type SessionActorQueue = ActorQueue;
/// Per-slot serialization queue (dashboard, slot-keyed).
///
/// Slot turns acquire on `slot_id` rather than `session_id` so that two
/// slots sharing a memory session do not serialize unnecessarily, and a
/// slot's per-slot agent state is guarded against concurrent turns.
pub type SlotActorQueue = ActorQueue;

/// Per-key serialization queue. Internal type shared by
/// [`SessionActorQueue`] and [`SlotActorQueue`].
pub struct ActorQueue {
    slots: Mutex<HashMap<String, Arc<QueueSlot>>>,
    max_queue_depth: usize,
    lock_timeout: Duration,
    idle_ttl: Duration,
}

struct QueueSlot {
    semaphore: Arc<Semaphore>,
    last_active: Mutex<Instant>,
    pending: AtomicUsize,
}

/// RAII guard that releases the permit on drop.
pub type SessionGuard = QueueGuard;
/// RAII guard that releases the slot permit on drop.
pub type SlotGuard = QueueGuard;

/// RAII guard returned by [`ActorQueue::acquire`].
pub struct QueueGuard {
    slot: Arc<QueueSlot>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for QueueGuard {
    fn drop(&mut self) {
        self.slot.pending.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Errors from an actor queue.
pub type SessionQueueError = ActorQueueError;
/// Errors from the slot actor queue. Alias for [`ActorQueueError`].
pub type SlotQueueError = ActorQueueError;

/// Errors from [`ActorQueue::acquire`].
#[derive(Debug)]
pub enum ActorQueueError {
    /// Too many requests queued for this key.
    QueueFull { key: String, depth: usize },
    /// Timed out waiting for the lock.
    Timeout { key: String },
}

impl std::fmt::Display for ActorQueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull { key, depth } => {
                write!(f, "Key {key} queue full ({depth} pending requests)")
            }
            Self::Timeout { key } => {
                write!(f, "Timed out waiting for key {key}")
            }
        }
    }
}

impl std::error::Error for ActorQueueError {}

impl ActorQueue {
    /// Create a new queue with the given limits.
    pub fn new(max_queue_depth: usize, lock_timeout_secs: u64, idle_ttl_secs: u64) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            max_queue_depth,
            lock_timeout: Duration::from_secs(lock_timeout_secs),
            idle_ttl: Duration::from_secs(idle_ttl_secs),
        }
    }

    /// Acquire exclusive access for `key`. Blocks until the key is free
    /// or the timeout expires. Returns a guard that releases on drop.
    pub async fn acquire(&self, key: &str) -> Result<QueueGuard, ActorQueueError> {
        let slot = {
            let mut slots = self.slots.lock().await;
            slots
                .entry(key.to_string())
                .or_insert_with(|| {
                    Arc::new(QueueSlot {
                        semaphore: Arc::new(Semaphore::new(1)),
                        last_active: Mutex::new(Instant::now()),
                        pending: AtomicUsize::new(0),
                    })
                })
                .clone()
        };

        // Check queue depth before waiting
        let current = slot.pending.fetch_add(1, Ordering::Relaxed);
        if current >= self.max_queue_depth {
            slot.pending.fetch_sub(1, Ordering::Relaxed);
            return Err(ActorQueueError::QueueFull {
                key: key.to_string(),
                depth: current,
            });
        }

        // Acquire owned permit with timeout
        let sem = slot.semaphore.clone();
        match tokio::time::timeout(self.lock_timeout, sem.acquire_owned()).await {
            Ok(Ok(permit)) => {
                *slot.last_active.lock().await = Instant::now();
                Ok(QueueGuard {
                    slot,
                    _permit: permit,
                })
            }
            Ok(Err(_)) | Err(_) => {
                slot.pending.fetch_sub(1, Ordering::Relaxed);
                Err(ActorQueueError::Timeout {
                    key: key.to_string(),
                })
            }
        }
    }

    /// Get the number of pending requests for `key`.
    pub async fn queue_depth(&self, key: &str) -> usize {
        let slots = self.slots.lock().await;
        slots
            .get(key)
            .map(|s| s.pending.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Remove idle slots that haven't been accessed within the TTL.
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
        let handle = tokio::spawn(async move { queue_clone.acquire("s1").await });

        // Give the spawned task time to register
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Third request should be rejected (pending=2 >= max=2)
        let result = queue.acquire("s1").await;
        assert!(matches!(result, Err(ActorQueueError::QueueFull { .. })));

        drop(guard);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn timeout_returns_error() {
        let queue = SessionActorQueue::new(8, 1, 600);
        let _guard = queue.acquire("s1").await.unwrap();

        let start = Instant::now();
        let result = queue.acquire("s1").await;
        assert!(matches!(result, Err(ActorQueueError::Timeout { .. })));
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

    #[tokio::test]
    async fn queue_depth_reports_correctly() {
        let queue = SessionActorQueue::new(8, 30, 600);
        assert_eq!(queue.queue_depth("s1").await, 0);

        let guard = queue.acquire("s1").await.unwrap();
        assert_eq!(queue.queue_depth("s1").await, 1);

        drop(guard);
        assert_eq!(queue.queue_depth("s1").await, 0);
    }

    // ── SlotActorQueue (alias) tests ─────────────────────────────────
    //
    // SlotActorQueue is a type alias over the same `ActorQueue` impl, so
    // behavioral guarantees are identical. The tests below pin the
    // slot-keyed API shape so renaming or separating the types later
    // surfaces compile errors at call sites rather than silent drift.

    #[tokio::test]
    async fn slot_queue_serializes_same_slot() {
        let queue = SlotActorQueue::new(8, 5, 600);
        let g1 = queue.acquire("slot-1").await.unwrap();
        drop(g1);
        let _g2: SlotGuard = queue.acquire("slot-1").await.unwrap();
    }

    #[tokio::test]
    async fn slot_queue_parallelizes_different_slots() {
        let queue = SlotActorQueue::new(8, 5, 600);
        let _g1 = queue.acquire("slot-a").await.unwrap();
        let _g2 = queue.acquire("slot-b").await.unwrap();
        // Two different slots can hold guards concurrently.
    }
}
