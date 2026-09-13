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
    /// Per-session incarnation counters. Deletion advances the counter while
    /// holding the session queue; long-lived holders use it to reject writes
    /// from a predecessor after an ID is reused.
    generations: Mutex<HashMap<String, u64>>,
    max_queue_depth: usize,
    lock_timeout: Duration,
    idle_ttl: Duration,
    #[cfg(test)]
    registration_hook: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    lease_registration_hook: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct SessionSlot {
    semaphore: Arc<Semaphore>,
    last_active: Mutex<Instant>,
    pending: AtomicUsize,
    leases: AtomicUsize,
}

/// RAII guard that releases the session permit on drop.
pub struct SessionGuard {
    _permit: OwnedSemaphorePermit,
    _registration: PendingRegistration,
}

/// A non-exclusive reference to a session incarnation held by a long-lived
/// caller. While this lease exists, idle eviction keeps the queue slot and its
/// generation tombstone so the caller cannot mistake an ID reuse for its
/// original session.
pub struct SessionLease {
    _registration: LeaseRegistration,
}

struct PendingRegistration {
    slot: Arc<SessionSlot>,
}

struct LeaseRegistration {
    slot: Arc<SessionSlot>,
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        self.slot.pending.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Drop for LeaseRegistration {
    fn drop(&mut self) {
        self.slot.leases.fetch_sub(1, Ordering::Relaxed);
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
            generations: Mutex::new(HashMap::new()),
            max_queue_depth,
            lock_timeout: Duration::from_secs(lock_timeout_secs),
            idle_ttl: Duration::from_secs(idle_ttl_secs),
            #[cfg(test)]
            registration_hook: std::sync::Mutex::new(None),
            #[cfg(test)]
            lease_registration_hook: std::sync::Mutex::new(None),
        }
    }

    /// Return the current lifecycle incarnation for a session key.
    pub async fn generation(&self, session_id: &str) -> u64 {
        self.generations
            .lock()
            .await
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    /// Capture a session incarnation only if a caller's synchronous durable
    /// predicate still holds. The predicate runs while the generation mutex is
    /// held, serializing its observation with [`Self::invalidate`]. This is
    /// intended for admission paths that must bind a durable existence check to
    /// the exact queue incarnation before they await the actor permit.
    pub async fn capture_generation_if<F>(&self, session_id: &str, predicate: F) -> Option<u64>
    where
        F: FnOnce() -> bool,
    {
        let generations = self.generations.lock().await;
        predicate().then(|| generations.get(session_id).copied().unwrap_or(0))
    }

    /// Invalidate all holders of the current incarnation. Must be called
    /// while holding this session's [`SessionGuard`].
    pub async fn invalidate(&self, session_id: &str) -> u64 {
        let mut generations = self.generations.lock().await;
        let generation = generations.entry(session_id.to_string()).or_insert(0);
        *generation = generation.wrapping_add(1);
        *generation
    }

    /// Acquire exclusive access to a session. Blocks until the session is free
    /// or the timeout expires. Returns a guard that releases on drop.
    pub async fn acquire(&self, session_id: &str) -> Result<SessionGuard, SessionQueueError> {
        let registration = {
            let mut slots = self.slots.lock().await;
            let slot = slots
                .entry(session_id.to_string())
                .or_insert_with(|| {
                    Arc::new(SessionSlot {
                        semaphore: Arc::new(Semaphore::new(1)),
                        last_active: Mutex::new(Instant::now()),
                        pending: AtomicUsize::new(0),
                        leases: AtomicUsize::new(0),
                    })
                })
                .clone();

            #[cfg(test)]
            let registration_hook = match self.registration_hook.lock() {
                Ok(hook) => hook,
                Err(poisoned) => poisoned.into_inner(),
            };
            #[cfg(test)]
            if let Some(hook) = registration_hook.as_ref() {
                hook();
            }

            // Register while holding the map lock so idle eviction cannot
            // remove this slot before it sees the pending request.
            let current = slot.pending.fetch_add(1, Ordering::Relaxed);
            let registration = PendingRegistration { slot };
            if current >= self.max_queue_depth {
                return Err(SessionQueueError::QueueFull {
                    session_id: session_id.to_string(),
                    depth: current,
                });
            }

            registration
        };

        // Acquire owned permit with timeout
        let sem = registration.slot.semaphore.clone();
        match tokio::time::timeout(self.lock_timeout, sem.acquire_owned()).await {
            Ok(Ok(permit)) => {
                *registration.slot.last_active.lock().await = Instant::now();
                Ok(SessionGuard {
                    _permit: permit,
                    _registration: registration,
                })
            }
            Ok(Err(_)) | Err(_) => Err(SessionQueueError::Timeout {
                session_id: session_id.to_string(),
            }),
        }
    }

    /// Retain a session's lifecycle incarnation without taking its actor
    /// permit. This is for a connection that stores a generation across many
    /// intermittent turns, such as a WebSocket. It deliberately does not
    /// consume queue-depth capacity: it is not an in-flight or queued turn.
    pub async fn retain(&self, session_id: &str) -> SessionLease {
        let slot = {
            let mut slots = self.slots.lock().await;
            let slot = slots
                .entry(session_id.to_string())
                .or_insert_with(|| {
                    Arc::new(SessionSlot {
                        semaphore: Arc::new(Semaphore::new(1)),
                        last_active: Mutex::new(Instant::now()),
                        pending: AtomicUsize::new(0),
                        leases: AtomicUsize::new(0),
                    })
                })
                .clone();
            // Register before releasing the slot-map mutex, just as acquire
            // registers pending work. Otherwise eviction could detach this
            // slot and reset its generation between lookup and registration.
            slot.leases.fetch_add(1, Ordering::Relaxed);

            #[cfg(test)]
            {
                let lease_registration_hook = match self.lease_registration_hook.lock() {
                    Ok(hook) => hook,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if let Some(hook) = lease_registration_hook.as_ref() {
                    hook();
                }
            }
            slot
        };
        SessionLease {
            _registration: LeaseRegistration { slot },
        }
    }

    #[cfg(test)]
    fn set_registration_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.registration_hook.lock().unwrap() = Some(hook);
    }

    #[cfg(test)]
    fn set_lease_registration_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.lease_registration_hook.lock().unwrap() = Some(hook);
    }

    /// Get the number of pending requests for a session.
    pub async fn queue_depth(&self, session_id: &str) -> usize {
        let slots = self.slots.lock().await;
        slots
            .get(session_id)
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
            if now.duration_since(last) > ttl
                && slot.pending.load(Ordering::Relaxed) == 0
                && slot.leases.load(Ordering::Relaxed) == 0
            {
                to_remove.push(key.clone());
            }
        }
        for key in &to_remove {
            slots.remove(key);
        }

        // A slot with no pending holders has no queued or active operation
        // that can still observe its incarnation. Reclaim its tombstone with
        // the slot so attacker-controlled, one-shot session keys cannot grow
        // the process-global generation map indefinitely.
        if !to_remove.is_empty() {
            let mut generations = self.generations.lock().await;
            for key in &to_remove {
                generations.remove(key);
            }
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
    async fn invalidation_advances_the_session_incarnation() {
        let queue = SessionActorQueue::new(8, 5, 600);
        assert_eq!(queue.generation("s1").await, 0);
        let guard = queue.acquire("s1").await.unwrap();
        assert_eq!(queue.invalidate("s1").await, 1);
        drop(guard);
        assert_eq!(queue.generation("s1").await, 1);
    }

    #[tokio::test]
    async fn generation_reads_do_not_retain_unseen_session_ids() {
        let queue = SessionActorQueue::new(8, 5, 600);
        for i in 0..128 {
            assert_eq!(queue.generation(&format!("unseen-{i}")).await, 0);
        }
        assert!(queue.generations.lock().await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn capture_generation_if_serializes_the_predicate_with_invalidation() {
        let queue = Arc::new(SessionActorQueue::new(8, 5, 600));
        let initial_guard = queue.acquire("s1").await.unwrap();
        assert_eq!(queue.invalidate("s1").await, 1);
        drop(initial_guard);

        let predicate_entered = Arc::new(std::sync::Barrier::new(2));
        let predicate_release = Arc::new(std::sync::Barrier::new(2));
        let snapshot_queue = Arc::clone(&queue);
        let entered = Arc::clone(&predicate_entered);
        let release = Arc::clone(&predicate_release);
        let snapshot = zeroclaw_spawn::spawn!(async move {
            snapshot_queue
                .capture_generation_if("s1", move || {
                    entered.wait();
                    release.wait();
                    true
                })
                .await
        });

        tokio::task::spawn_blocking(move || predicate_entered.wait())
            .await
            .unwrap();

        let invalidate_queue = Arc::clone(&queue);
        let mut invalidation = zeroclaw_spawn::spawn!(async move {
            let _guard = invalidate_queue.acquire("s1").await.unwrap();
            invalidate_queue.invalidate("s1").await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut invalidation)
                .await
                .is_err(),
            "invalidation must not advance while the durable predicate and its generation snapshot are coupled"
        );

        tokio::task::spawn_blocking(move || predicate_release.wait())
            .await
            .unwrap();
        assert_eq!(snapshot.await.unwrap(), Some(1));
        assert_eq!(invalidation.await.unwrap(), 2);
        assert_eq!(queue.generation("s1").await, 2);
    }

    #[tokio::test]
    async fn idle_eviction_reclaims_invalidated_generation_tombstones() {
        let queue = SessionActorQueue::new(8, 5, 0);
        let guard = queue.acquire("deleted-session").await.unwrap();
        queue.invalidate("deleted-session").await;
        drop(guard);

        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(queue.evict_idle().await, 1);
        assert_eq!(queue.generation("deleted-session").await, 0);
        assert!(queue.generations.lock().await.is_empty());
    }

    #[tokio::test]
    async fn lifecycle_lease_keeps_a_generation_until_its_long_lived_holder_exits() {
        let queue = SessionActorQueue::new(8, 5, 0);
        let guard = queue.acquire("connected-session").await.unwrap();
        queue.invalidate("connected-session").await;
        drop(guard);
        let lease = queue.retain("connected-session").await;

        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(queue.evict_idle().await, 0);
        assert_eq!(queue.generation("connected-session").await, 1);

        drop(lease);
        assert_eq!(queue.evict_idle().await, 1);
        assert_eq!(queue.generation("connected-session").await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lifecycle_lease_registration_is_atomic_with_idle_eviction() {
        let queue = Arc::new(SessionActorQueue::new(8, 5, 0));
        let guard = queue.acquire("connected-session").await.unwrap();
        queue.invalidate("connected-session").await;
        drop(guard);
        tokio::time::sleep(Duration::from_millis(1)).await;

        let selected = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let hook_selected = Arc::clone(&selected);
        let hook_resume = Arc::clone(&resume);
        queue.set_lease_registration_hook(Arc::new(move || {
            hook_selected.wait();
            hook_resume.wait();
        }));

        let retain_queue = Arc::clone(&queue);
        let retain =
            zeroclaw_spawn::spawn!(async move { retain_queue.retain("connected-session").await });
        tokio::task::spawn_blocking(move || selected.wait())
            .await
            .unwrap();

        let evict_queue = Arc::clone(&queue);
        let mut eviction = zeroclaw_spawn::spawn!(async move { evict_queue.evict_idle().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut eviction)
                .await
                .is_err(),
            "eviction must not observe an unregistered lifecycle lease"
        );
        tokio::task::spawn_blocking(move || resume.wait())
            .await
            .unwrap();

        let lease = retain.await.unwrap();
        assert_eq!(eviction.await.unwrap(), 0);
        assert_eq!(queue.generation("connected-session").await, 1);
        drop(lease);
        assert_eq!(queue.evict_idle().await, 1);
        assert_eq!(queue.generation("connected-session").await, 0);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registration_and_eviction_are_atomic() {
        let queue = Arc::new(SessionActorQueue::new(8, 5, 0));
        let selected = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let hook_selected = selected.clone();
        let hook_resume = resume.clone();
        queue.set_registration_hook(Arc::new(move || {
            hook_selected.wait();
            hook_resume.wait();
        }));

        let acquire_queue = queue.clone();
        let acquire = zeroclaw_spawn::spawn!(async move { acquire_queue.acquire("s1").await });
        tokio::task::spawn_blocking(move || selected.wait())
            .await
            .unwrap();

        let evict_queue = queue.clone();
        let mut eviction = zeroclaw_spawn::spawn!(async move { evict_queue.evict_idle().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut eviction)
                .await
                .is_err(),
            "eviction must wait for request registration to release the slot map"
        );
        tokio::task::spawn_blocking(move || resume.wait())
            .await
            .unwrap();

        let guard = acquire.await.unwrap().unwrap();
        assert_eq!(eviction.await.unwrap(), 0);
        assert_eq!(queue.queue_depth("s1").await, 1);

        drop(guard);
        assert_eq!(queue.evict_idle().await, 1);
    }

    #[tokio::test]
    async fn cancelled_waiter_releases_registration() {
        let queue = Arc::new(SessionActorQueue::new(8, 30, 0));
        let guard = queue.acquire("s1").await.unwrap();

        let waiter_queue = queue.clone();
        let waiter = zeroclaw_spawn::spawn!(async move { waiter_queue.acquire("s1").await });
        while queue.queue_depth("s1").await < 2 {
            tokio::task::yield_now().await;
        }

        waiter.abort();
        let _ = waiter.await;
        assert_eq!(queue.queue_depth("s1").await, 1);

        drop(guard);
        assert_eq!(queue.evict_idle().await, 1);
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
}
