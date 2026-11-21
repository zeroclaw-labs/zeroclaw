//! Per-session actor queue for serializing concurrent access.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// Per-session serialization queue.
pub struct SessionActorQueue {
    slots: Mutex<HashMap<String, Arc<SessionSlot>>>,
    max_queue_depth: usize,
    lock_timeout: Duration,
    idle_ttl: Duration,
    /// Per-session transcript generation. Every gateway write to a session's
    /// transcript, and its deletion, advances it, so a connection holding
    /// history can tell that the transcript changed under it even when the
    /// message count did not. Not subject to idle eviction: dropping an entry
    /// would let a stale holder match again.
    generations: std::sync::Mutex<HashMap<String, u64>>,
    next_generation: AtomicU64,
    #[cfg(test)]
    registration_hook: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct SessionSlot {
    semaphore: Arc<Semaphore>,
    last_active: Mutex<Instant>,
    pending: AtomicUsize,
    eviction_reserved: std::sync::atomic::AtomicBool,
}

/// RAII guard that releases the session permit on drop.
pub struct SessionGuard {
    _permit: OwnedSemaphorePermit,
    _registration: PendingRegistration,
    /// The session this guard admits. Lets admission-aware helpers (e.g.
    /// `SessionStore::insert_admitted`) verify the caller holds the permit
    /// for the session it is about to mutate.
    session_id: String,
}

/// A FIFO position registered on a session without waiting for its permit.
///
/// The semaphore waiter is polled during reservation so later requests cannot
/// overtake this admission while the caller performs lifecycle work. The
/// permit may already be held if it was immediately available. The queue-lock
/// timeout begins when [`Self::acquire`] is called, matching the existing
/// `SessionActorQueue::acquire` contract.
pub struct SessionQueueAdmission {
    registration: PendingRegistration,
    session_id: String,
    lock_timeout: Duration,
    permit: Option<OwnedSemaphorePermit>,
    waiter: Option<SemaphoreAcquire>,
}

type SemaphoreAcquire =
    Pin<Box<dyn Future<Output = Result<OwnedSemaphorePermit, tokio::sync::AcquireError>> + Send>>;

impl SessionQueueAdmission {
    /// Wait for this reservation's FIFO turn and acquire its session permit.
    pub async fn acquire(self) -> Result<SessionGuard, SessionQueueError> {
        let Self {
            registration,
            session_id,
            lock_timeout,
            permit,
            waiter,
        } = self;
        let permit = match permit {
            Some(permit) => Ok(permit),
            None => match tokio::time::timeout(
                lock_timeout,
                waiter.expect("pending reservation must retain its semaphore waiter"),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    return Err(SessionQueueError::Timeout { session_id });
                }
            },
        };
        match permit {
            Ok(permit) => {
                *registration.slot.last_active.lock().await = Instant::now();
                Ok(SessionGuard {
                    _permit: permit,
                    _registration: registration,
                    session_id,
                })
            }
            Err(_) => Err(SessionQueueError::Timeout { session_id }),
        }
    }
}

/// Nonblocking admission reservation used while a session is being evicted.
pub struct SessionEvictionGuard {
    slot: Arc<SessionSlot>,
    _permit: OwnedSemaphorePermit,
    _registration: PendingRegistration,
    session_id: String,
}

impl SessionEvictionGuard {
    /// The session id whose idle queue this eviction owns.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl Drop for SessionEvictionGuard {
    fn drop(&mut self) {
        self.slot.eviction_reserved.store(false, Ordering::Release);
    }
}

impl SessionGuard {
    /// The session id whose admission permit this guard owns.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
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
    /// The session is at the short, atomic eviction boundary.
    Evicting { session_id: String },
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
            Self::Evicting { session_id } => {
                write!(f, "Session {session_id} is being removed")
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
            generations: std::sync::Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(0),
            #[cfg(test)]
            registration_hook: std::sync::Mutex::new(None),
        }
    }

    /// The session's current transcript generation; `0` until first written
    /// through [`Self::advance_generation`].
    pub fn generation(&self, session_id: &str) -> u64 {
        self.generations
            .lock()
            .map(|map| map.get(session_id).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    /// Record a write to, or deletion of, a session's transcript and return
    /// the new generation. Values are unique across the queue, so a
    /// generation never recurs for a key once superseded.
    pub fn advance_generation(&self, session_id: &str) -> u64 {
        let next = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut map) = self.generations.lock() {
            map.insert(session_id.to_string(), next);
        }
        next
    }

    /// Acquire exclusive access to a session. Blocks until the session is free
    /// or the timeout expires. Returns a guard that releases on drop.
    pub async fn acquire(&self, session_id: &str) -> Result<SessionGuard, SessionQueueError> {
        self.reserve(session_id).await?.acquire().await
    }

    /// Try exclusive session access without waiting for the turn permit.
    ///
    /// Uses the same FIFO registration, depth limit and eviction checks as
    /// [`Self::reserve`]. A busy session returns `Ok(None)` and retains no
    /// waiter or queue capacity. Acquiring the internal bookkeeping locks may
    /// suspend; this does not wait for the active session turn to end.
    pub async fn try_acquire(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionGuard>, SessionQueueError> {
        let admission = self.reserve(session_id).await?;
        if admission.permit.is_none() {
            return Ok(None);
        }
        admission.acquire().await.map(Some)
    }

    /// Register a session admission in semaphore FIFO order without waiting
    /// for the permit. This lets lifecycle callers reserve their place before
    /// draining work that may itself need the session permit to finish.
    pub async fn reserve(
        &self,
        session_id: &str,
    ) -> Result<SessionQueueAdmission, SessionQueueError> {
        let registration = {
            let mut slots = self.slots.lock().await;
            let slot = slots
                .entry(session_id.to_string())
                .or_insert_with(|| {
                    Arc::new(SessionSlot {
                        semaphore: Arc::new(Semaphore::new(1)),
                        last_active: Mutex::new(Instant::now()),
                        pending: AtomicUsize::new(0),
                        eviction_reserved: std::sync::atomic::AtomicBool::new(false),
                    })
                })
                .clone();

            if slot.eviction_reserved.load(Ordering::Acquire) {
                return Err(SessionQueueError::Evicting {
                    session_id: session_id.to_string(),
                });
            }

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

        let mut waiter: SemaphoreAcquire =
            Box::pin(Arc::clone(&registration.slot.semaphore).acquire_owned());
        let first_poll =
            std::future::poll_fn(|context| Poll::Ready(waiter.as_mut().poll(context))).await;
        let (permit, waiter) = match first_poll {
            Poll::Ready(Ok(permit)) => (Some(permit), None),
            Poll::Ready(Err(_)) => {
                return Err(SessionQueueError::Timeout {
                    session_id: session_id.to_string(),
                });
            }
            Poll::Pending => (None, Some(waiter)),
        };
        Ok(SessionQueueAdmission {
            registration,
            session_id: session_id.to_string(),
            lock_timeout: self.lock_timeout,
            permit,
            waiter,
        })
    }

    /// Reserve an idle session for eviction without waiting. The reservation
    /// is linearized under the slot-map lock: earlier admissions make this
    /// return `None`, and later admissions receive `SessionQueueError::Evicting`
    /// until the returned guard is dropped.
    pub async fn try_begin_eviction(&self, session_id: &str) -> Option<SessionEvictionGuard> {
        let mut slots = self.slots.lock().await;
        let slot = slots
            .entry(session_id.to_string())
            .or_insert_with(|| {
                Arc::new(SessionSlot {
                    semaphore: Arc::new(Semaphore::new(1)),
                    last_active: Mutex::new(Instant::now()),
                    pending: AtomicUsize::new(0),
                    eviction_reserved: std::sync::atomic::AtomicBool::new(false),
                })
            })
            .clone();

        if self.max_queue_depth == 0
            || slot.pending.load(Ordering::Acquire) != 0
            || slot
                .eviction_reserved
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }

        slot.pending.fetch_add(1, Ordering::AcqRel);
        let registration = PendingRegistration {
            slot: Arc::clone(&slot),
        };
        let permit = match Arc::clone(&slot.semaphore).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                slot.eviction_reserved.store(false, Ordering::Release);
                drop(registration);
                return None;
            }
        };
        let guard = SessionEvictionGuard {
            slot,
            _permit: permit,
            _registration: registration,
            session_id: session_id.to_string(),
        };
        drop(slots);
        *guard.slot.last_active.lock().await = Instant::now();
        Some(guard)
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
    async fn try_acquire_retains_the_existing_session_permit() {
        let queue = SessionActorQueue::new(8, 5, 600);
        let guard = queue.try_acquire("s1").await.unwrap().unwrap();
        assert_eq!(guard.session_id(), "s1");
        assert_eq!(queue.queue_depth("s1").await, 1);
        assert!(queue.try_acquire("s1").await.unwrap().is_none());
        assert_eq!(queue.queue_depth("s1").await, 1);
        assert!(queue.try_begin_eviction("s1").await.is_none());
        drop(guard);
        assert_eq!(queue.queue_depth("s1").await, 0);
        assert!(queue.try_acquire("s1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn try_acquire_busy_drops_its_waiter_without_waiting_for_timeout() {
        let queue = SessionActorQueue::new(8, 30, 600);
        let active = queue.acquire("s1").await.unwrap();
        for _ in 0..8 {
            assert!(
                tokio::time::timeout(Duration::from_millis(100), queue.try_acquire("s1"))
                    .await
                    .expect("a busy try must not wait for the turn permit")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(queue.queue_depth("s1").await, 1);
        }
        drop(active);
        assert!(queue.try_acquire("s1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn try_acquire_respects_depth_and_eviction_refusals() {
        let queue = SessionActorQueue::new(1, 5, 600);
        let active = queue.acquire("s1").await.unwrap();
        assert!(matches!(
            queue.try_acquire("s1").await,
            Err(SessionQueueError::QueueFull { session_id, depth: 1 }) if session_id == "s1"
        ));
        assert_eq!(queue.queue_depth("s1").await, 1);
        drop(active);
        let eviction = queue.try_begin_eviction("s1").await.unwrap();
        assert!(matches!(
            queue.try_acquire("s1").await,
            Err(SessionQueueError::Evicting { session_id }) if session_id == "s1"
        ));
        assert_eq!(queue.queue_depth("s1").await, 1);
        drop(eviction);
        assert_eq!(queue.queue_depth("s1").await, 0);
        assert!(queue.try_acquire("s1").await.unwrap().is_some());

        let disabled = SessionActorQueue::new(0, 5, 600);
        assert!(matches!(
            disabled.try_acquire("s1").await,
            Err(SessionQueueError::QueueFull { depth: 0, .. })
        ));
        assert_eq!(disabled.queue_depth("s1").await, 0);
    }

    #[tokio::test]
    async fn try_acquire_cannot_overtake_a_reserved_fifo_position() {
        let queue = SessionActorQueue::new(8, 5, 600);
        let active = queue.acquire("s1").await.unwrap();
        let earlier = queue.reserve("s1").await.unwrap();
        drop(active);
        assert!(queue.try_acquire("s1").await.unwrap().is_none());
        assert_eq!(queue.queue_depth("s1").await, 1);
        let guard = earlier.acquire().await.unwrap();
        assert!(queue.try_acquire("s1").await.unwrap().is_none());
        assert_eq!(queue.queue_depth("s1").await, 1);
        drop(guard);
        assert!(queue.try_acquire("s1").await.unwrap().is_some());
    }

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
    async fn admission_reservation_preserves_fifo_before_permit_wait() {
        let queue = Arc::new(SessionActorQueue::new(8, 5, 600));
        let active = queue.acquire("s1").await.unwrap();
        let reservation = queue.reserve("s1").await.unwrap();

        let later_queue = Arc::clone(&queue);
        let mut later = zeroclaw_spawn::spawn!(async move { later_queue.acquire("s1").await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while queue.queue_depth("s1").await < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("later admission must register behind the reserved FIFO position");

        drop(active);
        let first = tokio::time::timeout(Duration::from_secs(1), reservation.acquire())
            .await
            .expect("reserved admission should receive the next permit")
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut later)
                .await
                .is_err(),
            "a later ordinary admission must not overtake the reservation"
        );

        drop(first);
        tokio::time::timeout(Duration::from_secs(1), later)
            .await
            .expect("later admission should proceed after the reservation releases")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn dropping_admission_reservation_releases_queue_capacity() {
        let queue = SessionActorQueue::new(2, 5, 600);
        let active = queue.acquire("s1").await.unwrap();
        let reservation = queue.reserve("s1").await.unwrap();
        assert_eq!(queue.queue_depth("s1").await, 2);
        assert!(matches!(
            queue.reserve("s1").await,
            Err(SessionQueueError::QueueFull { depth: 2, .. })
        ));

        drop(reservation);
        assert_eq!(queue.queue_depth("s1").await, 1);
        drop(active);
        let _admission = queue.reserve("s1").await.unwrap();
    }

    #[tokio::test]
    async fn eviction_reservation_rejects_new_admissions_until_removal_finishes() {
        let queue = SessionActorQueue::new(8, 5, 600);
        let eviction = queue.try_begin_eviction("s1").await.unwrap();
        assert_eq!(queue.queue_depth("s1").await, 1);
        assert!(matches!(
            queue.acquire("s1").await,
            Err(SessionQueueError::Evicting { ref session_id }) if session_id == "s1"
        ));

        drop(eviction);
        let _guard = queue.acquire("s1").await.unwrap();
    }

    #[tokio::test]
    async fn eviction_reservation_skips_active_or_queued_work() {
        let queue = Arc::new(SessionActorQueue::new(8, 5, 600));
        let active = queue.acquire("s1").await.unwrap();
        assert!(queue.try_begin_eviction("s1").await.is_none());

        let queue_clone = Arc::clone(&queue);
        let waiting = zeroclaw_spawn::spawn!(async move { queue_clone.acquire("s1").await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(queue.try_begin_eviction("s1").await.is_none());
        drop(active);
        drop(waiting.await.unwrap().unwrap());
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

    #[test]
    fn transcript_generation_advances_uniquely_per_session() {
        let queue = SessionActorQueue::new(8, 30, 600);
        assert_eq!(queue.generation("gw_a"), 0);
        let first = queue.advance_generation("gw_a");
        assert_eq!(queue.generation("gw_a"), first);
        let second = queue.advance_generation("gw_a");
        assert!(second > first, "a superseded generation never recurs");
        assert_eq!(queue.generation("gw_b"), 0, "generations are per session");
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
