//! Process-local, per-conversation foreground coordination.
//!
//! The channel orchestrator owns the canonical conversation key and chooses
//! which channel paths acquire the foreground lease. This module holds no
//! session, authority, or durable Goal state.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio_util::sync::CancellationToken;
use zeroclaw_goal::GoalExecutionSupervisor;

pub(crate) struct ConversationLocks {
    persist: Arc<Mutex<()>>,
    matrix_goal: OnceLock<MatrixGoalLocks>,
    // Isolated ingress fixtures use the real store/coordinator handle without
    // initializing the daemon singleton shared by unrelated unit tests.
    #[cfg(test)]
    goal_test_control_plane: OnceLock<zeroclaw_runtime::control_plane::ControlPlaneHandle>,
    #[cfg(test)]
    goal_test_bind_barrier: Mutex<Option<Arc<GoalTestBindBarrier>>>,
    #[cfg(test)]
    goal_test_drop_reset_completion: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    goal_test_cancel_reset_admission: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    goal_test_drop_reset_admission: std::sync::atomic::AtomicBool,
}

/// A one-shot fixture barrier at the real driver's admission boundary.
#[cfg(test)]
#[derive(Default)]
pub(super) struct GoalTestBindBarrier {
    pub(super) entered: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
    fail_after_release: bool,
}

/// Locks required only after a Matrix conversation enters the Goal control
/// path. Ordinary channel traffic retains the historical persistence lock
/// without paying for unused async mutexes and coordination counters.
struct MatrixGoalLocks {
    goal_command: Arc<tokio::sync::Mutex<()>>,
    foreground: Arc<tokio::sync::Mutex<()>>,
    goal_supervisor: Arc<tokio::sync::Mutex<Option<Arc<GoalExecutionSupervisor>>>>,
    pending_resets: Arc<AtomicUsize>,
    pending_stops: Arc<AtomicUsize>,
}

const MAX_PENDING_MATRIX_GOAL_STOPS: usize = super::MAX_CONCURRENT_STOP_REPLIES;

/// Owns one bounded command-lock waiter, not a durable Goal transition.
pub(super) struct MatrixGoalStopGuard {
    pending_stops: Arc<AtomicUsize>,
}

impl Drop for MatrixGoalStopGuard {
    fn drop(&mut self) {
        self.pending_stops.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Canonical process-local reset participation, not durable Goal state.
/// A queued turn owns this reservation through reset delivery or abandonment.
pub(super) struct MatrixGoalResetGuard {
    pending_resets: Arc<AtomicUsize>,
}

impl Drop for MatrixGoalResetGuard {
    fn drop(&mut self) {
        self.pending_resets.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ConversationLocks {
    pub(crate) fn new() -> Self {
        Self {
            persist: Arc::new(Mutex::new(())),
            matrix_goal: OnceLock::new(),
            #[cfg(test)]
            goal_test_control_plane: OnceLock::new(),
            #[cfg(test)]
            goal_test_bind_barrier: Mutex::new(None),
            #[cfg(test)]
            goal_test_drop_reset_completion: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            goal_test_cancel_reset_admission: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            goal_test_drop_reset_admission: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn matrix_goal(&self) -> &MatrixGoalLocks {
        self.matrix_goal.get_or_init(|| MatrixGoalLocks {
            goal_command: Arc::new(tokio::sync::Mutex::new(())),
            foreground: Arc::new(tokio::sync::Mutex::new(())),
            goal_supervisor: Arc::new(tokio::sync::Mutex::new(None)),
            pending_resets: Arc::new(AtomicUsize::new(0)),
            pending_stops: Arc::new(AtomicUsize::new(0)),
        })
    }

    #[cfg(test)]
    fn has_matrix_goal_locks(&self) -> bool {
        self.matrix_goal.get().is_some()
    }
}

#[cfg(test)]
pub(super) fn install_goal_test_control_plane(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
    handle: zeroclaw_runtime::control_plane::ControlPlaneHandle,
) {
    assert!(
        locks_for_key(locks, key)
            .goal_test_control_plane
            .set(handle)
            .is_ok()
    );
}

#[cfg(test)]
pub(super) fn goal_test_control_plane(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> Option<zeroclaw_runtime::control_plane::ControlPlaneHandle> {
    locks_for_key(locks, key)
        .goal_test_control_plane
        .get()
        .cloned()
}

#[cfg(test)]
pub(super) fn drop_next_goal_test_reset_completion(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) {
    assert!(
        !locks_for_key(locks, key)
            .goal_test_drop_reset_completion
            .swap(true, Ordering::SeqCst)
    );
}

#[cfg(test)]
pub(super) fn take_goal_test_drop_reset_completion(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> bool {
    locks_for_key(locks, key)
        .goal_test_drop_reset_completion
        .swap(false, Ordering::SeqCst)
}

#[cfg(test)]
pub(super) fn cancel_next_goal_test_reset_admission(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) {
    assert!(
        !locks_for_key(locks, key)
            .goal_test_cancel_reset_admission
            .swap(true, Ordering::SeqCst)
    );
}

#[cfg(test)]
pub(super) fn take_goal_test_cancel_reset_admission(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> bool {
    locks_for_key(locks, key)
        .goal_test_cancel_reset_admission
        .swap(false, Ordering::SeqCst)
}

#[cfg(test)]
pub(super) fn drop_next_goal_test_reset_admission(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) {
    assert!(
        !locks_for_key(locks, key)
            .goal_test_drop_reset_admission
            .swap(true, Ordering::SeqCst)
    );
}

#[cfg(test)]
pub(super) fn take_goal_test_drop_reset_admission(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> bool {
    locks_for_key(locks, key)
        .goal_test_drop_reset_admission
        .swap(false, Ordering::SeqCst)
}

#[cfg(test)]
pub(super) fn install_goal_test_bind_barrier(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
    fail_after_release: bool,
) -> Arc<GoalTestBindBarrier> {
    let barrier = Arc::new(GoalTestBindBarrier {
        fail_after_release,
        ..Default::default()
    });
    let entry = locks_for_key(locks, key);
    assert!(
        entry
            .goal_test_bind_barrier
            .lock()
            .unwrap()
            .replace(barrier.clone())
            .is_none()
    );
    barrier
}

#[cfg(test)]
pub(super) async fn await_goal_test_bind_barrier(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> anyhow::Result<()> {
    let entry = locks_for_key(locks, key);
    let barrier = entry.goal_test_bind_barrier.lock().unwrap().take();
    if let Some(barrier) = barrier {
        barrier.entered.notify_one();
        barrier.release.notified().await;
        if barrier.fail_after_release {
            anyhow::bail!("fixture launch rebind unavailable");
        }
    }
    Ok(())
}

fn locks_for_key(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> Arc<ConversationLocks> {
    let mut locks = locks.lock().unwrap_or_else(|error| error.into_inner());
    locks
        .entry(key.to_owned())
        .or_insert_with(|| Arc::new(ConversationLocks::new()))
        .clone()
}

pub(crate) fn persist_lock(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> Arc<Mutex<()>> {
    Arc::clone(&locks_for_key(locks, key).persist)
}

pub(crate) fn foreground_lock(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    Arc::clone(&locks_for_key(locks, key).matrix_goal().foreground)
}

pub(crate) fn goal_command_lock(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    Arc::clone(&locks_for_key(locks, key).matrix_goal().goal_command)
}

pub(super) fn reserve_matrix_goal_reset(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> MatrixGoalResetGuard {
    let pending_resets = Arc::clone(&locks_for_key(locks, key).matrix_goal().pending_resets);
    pending_resets.fetch_add(1, Ordering::SeqCst);
    MatrixGoalResetGuard { pending_resets }
}

pub(super) fn reserve_matrix_goal_stop(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> Option<MatrixGoalStopGuard> {
    let pending_stops = Arc::clone(&locks_for_key(locks, key).matrix_goal().pending_stops);
    pending_stops
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count < MAX_PENDING_MATRIX_GOAL_STOPS).then_some(count + 1)
        })
        .ok()?;
    Some(MatrixGoalStopGuard { pending_stops })
}

pub(super) fn matrix_goal_reset_pending(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> bool {
    locks_for_key(locks, key)
        .matrix_goal()
        .pending_resets
        .load(Ordering::SeqCst)
        != 0
}

pub(crate) fn goal_supervisor_slot(
    locks: &Arc<Mutex<HashMap<String, Arc<ConversationLocks>>>>,
    key: &str,
) -> Arc<tokio::sync::Mutex<Option<Arc<GoalExecutionSupervisor>>>> {
    Arc::clone(&locks_for_key(locks, key).matrix_goal().goal_supervisor)
}

pub(crate) async fn wait_for_foreground_lease(
    lock: Arc<tokio::sync::Mutex<()>>,
    cancellation: CancellationToken,
) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    tokio::select! {
        guard = lock.lock_owned() => Some(guard),
        () = cancellation.cancelled() => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn foreground_lease_serializes_one_conversation_only() {
        let locks = Arc::new(Mutex::new(HashMap::new()));
        let first = foreground_lock(&locks, "matrix:room:thread");
        let same = foreground_lock(&locks, "matrix:room:thread");
        let other = foreground_lock(&locks, "matrix:other-room:thread");
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other));

        let held = first.lock_owned().await;
        let mut same_waiter = zeroclaw_spawn::spawn!(async move { same.lock_owned().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut same_waiter)
                .await
                .is_err()
        );
        let other_guard =
            tokio::time::timeout(std::time::Duration::from_millis(50), other.lock_owned())
                .await
                .expect("independent conversations must not wait");
        drop(other_guard);
        drop(held);
        tokio::time::timeout(std::time::Duration::from_millis(500), same_waiter)
            .await
            .expect("same conversation must proceed after release")
            .expect("foreground waiter must not panic");
    }

    #[test]
    fn persistence_only_conversation_does_not_allocate_matrix_goal_locks() {
        let locks = Arc::new(Mutex::new(HashMap::new()));
        let _persist = persist_lock(&locks, "channel:ordinary");
        let entry = locks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get("channel:ordinary")
            .expect("persistence lookup creates the conversation entry")
            .clone();
        assert!(!entry.has_matrix_goal_locks());

        let _foreground = foreground_lock(&locks, "channel:ordinary");
        assert!(entry.has_matrix_goal_locks());
    }

    #[tokio::test]
    async fn cancelled_foreground_wait_never_acquires_a_lease() {
        let locks = Arc::new(Mutex::new(HashMap::new()));
        let lock = foreground_lock(&locks, "matrix:room:thread");
        let held = lock.clone().lock_owned().await;
        let cancellation = CancellationToken::new();
        let waiter_cancellation = cancellation.clone();
        let waiter = zeroclaw_spawn::spawn!(wait_for_foreground_lease(lock, waiter_cancellation));
        cancellation.cancel();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
                .await
                .expect("cancelled waiter must complete")
                .expect("foreground waiter must not panic")
                .is_none()
        );
        drop(held);
    }
}
