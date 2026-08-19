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
    #[cfg(test)]
    registration_hook: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct SessionSlot {
    semaphore: Arc<Semaphore>,
    last_active: Mutex<Instant>,
    pending: AtomicUsize,
}

/// RAII guard that releases the session permit on drop.
pub struct SessionGuard {
    _permit: OwnedSemaphorePermit,
    _registration: PendingRegistration,
}

struct PendingRegistration {
    slot: Arc<SessionSlot>,
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        self.slot.pending.fetch_sub(1, Ordering::Relaxed);
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
            #[cfg(test)]
            registration_hook: std::sync::Mutex::new(None),
        }
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
                    })
                })
                .clone();

            #[cfg(test)]
            if let Some(hook) = self.registration_hook.lock().unwrap().as_ref() {
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

    #[cfg(test)]
    fn set_registration_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.registration_hook.lock().unwrap() = Some(hook);
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
