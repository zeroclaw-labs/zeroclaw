//! Authoritative in-process session lifecycle state for queued gateway writers.
//!
//! Queued writers (both WebSocket prompt paths and the REST message-append
//! endpoint) wait on a `session_queue` permit. While they wait, an operator
//! can delete the session, and the turn holding the permit can still be
//! finalizing. Two questions must be answered *after* the permit is acquired,
//! and neither can be answered by probing the backend for existence:
//!
//! * "Was this session deleted while I was queued?" A backend existence probe
//!   conflates *destroyed* with *never created*: `SessionStore::session_exists`
//!   is a file-presence check, and a JSONL session file does not exist until
//!   its first append. Probing would reject the very first prompt of every new
//!   session as if it had been deleted.
//! * "Has the previous turn finished settling?" A turn's persistence and its
//!   turn-version bump happen after the turn's cancel token is dropped, so a
//!   liveness check alone reports the session idle while the transcript behind
//!   it is still being written.
//!
//! Both are lifecycle facts about the gateway's own bookkeeping rather than
//! facts about bytes on disk, so they are tracked here explicitly.
//!
//! The deletion counter is monotonic per session key: writers capture it
//! *before* waiting and compare *after* acquiring. Comparing generations
//! rather than reading a boolean "is deleted" flag keeps delete/recreate
//! cycles unambiguous — a session deleted and recreated while a writer queued
//! has a different generation even though it exists again at both ends of the
//! wait, and that writer's view of history is stale either way.

use std::collections::HashMap;
use std::sync::Mutex;

/// A session's deletion generation, captured before a writer starts waiting.
///
/// Compare with [`SessionLifecycle::deletion_generation`] after acquiring the
/// permit; any change means the session was deleted (and possibly recreated)
/// while the writer was queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletionGeneration(u64);

/// Tracks per-session deletion generations and in-progress finalization.
///
/// Entries are only created for sessions that are actually deleted or
/// finalized, so an untouched session costs nothing. A session that has never
/// been deleted reports generation 0, which is exactly what a writer that
/// captured its generation before the session existed also sees — the
/// "brand-new session" case that must not be mistaken for deletion.
#[derive(Debug, Default)]
pub struct SessionLifecycle {
    deletions: Mutex<HashMap<String, u64>>,
    finalizing: Mutex<HashMap<String, u64>>,
    persistence_failures: Mutex<std::collections::HashSet<String>>,
}

impl SessionLifecycle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `session_key`'s current deletion generation.
    ///
    /// Callers capture this *before* awaiting a `session_queue` permit and
    /// re-compare after acquiring it.
    #[must_use]
    pub fn deletion_generation(&self, session_key: &str) -> DeletionGeneration {
        let deletions = self.deletions.lock().expect("deletions lock poisoned");
        DeletionGeneration(deletions.get(session_key).copied().unwrap_or(0))
    }

    /// Record that `session_key` was deleted, invalidating every writer that
    /// captured a generation before this call.
    pub fn record_deletion(&self, session_key: &str) {
        let mut deletions = self.deletions.lock().expect("deletions lock poisoned");
        let next = deletions.get(session_key).copied().unwrap_or(0) + 1;
        deletions.insert(session_key.to_string(), next);
    }

    /// True when `session_key` was deleted since `captured` was read.
    #[must_use]
    pub fn deleted_since(&self, session_key: &str, captured: DeletionGeneration) -> bool {
        self.deletion_generation(session_key) != captured
    }

    /// Mark a turn as finalizing: it has stopped streaming but its messages
    /// are not yet persisted and its turn version is not yet resolved.
    ///
    /// Held as a count rather than a flag so overlapping finalizations (a
    /// cancelled turn unwinding while a delete-triggered cleanup runs) cannot
    /// clear each other's state early.
    pub fn begin_finalizing(&self, session_key: &str) {
        let mut finalizing = self.finalizing.lock().expect("finalizing lock poisoned");
        *finalizing.entry(session_key.to_string()).or_insert(0) += 1;
    }

    /// Release one finalization hold, removing the entry at zero so the map
    /// does not retain completed sessions forever.
    pub fn end_finalizing(&self, session_key: &str) {
        let mut finalizing = self.finalizing.lock().expect("finalizing lock poisoned");
        if let Some(count) = finalizing.get_mut(session_key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                finalizing.remove(session_key);
            }
        }
    }

    /// True while any turn for `session_key` is between "stopped streaming"
    /// and "persistence and turn-version disposition complete".
    ///
    /// The session-state endpoint treats this as still-running so a dashboard
    /// cannot hydrate a transcript that is mid-write.
    #[must_use]
    pub fn is_finalizing(&self, session_key: &str) -> bool {
        let finalizing = self.finalizing.lock().expect("finalizing lock poisoned");
        finalizing.contains_key(session_key)
    }

    /// Drop per-session bookkeeping that is meaningless once the session is
    /// gone. The deletion generation is deliberately *retained*: writers still
    /// queued need it to detect the deletion they slept through.
    pub fn forget_finalizing(&self, session_key: &str) {
        let mut finalizing = self.finalizing.lock().expect("finalizing lock poisoned");
        finalizing.remove(session_key);
    }

    /// Record that a turn's messages could not be fully persisted.
    ///
    /// Suppressing the turn-version bump alone is not enough to protect the
    /// next writer: with the version unchanged, a queued connection compares
    /// equal to its `seen_version`, concludes nothing has completed, skips
    /// rehydration, and runs against its pre-turn history — the transcript
    /// the failed append was supposed to extend. The gateway cannot repair
    /// the backend, so it records the damage explicitly and lets the next
    /// writer fail loudly instead of silently continuing from history known
    /// to be wrong.
    pub fn record_persistence_failure(&self, session_key: &str) {
        let mut failures = self
            .persistence_failures
            .lock()
            .expect("persistence failures lock poisoned");
        failures.insert(session_key.to_string());
    }

    /// True when a turn for `session_key` failed to persist and the session's
    /// transcript has not been re-read since.
    #[must_use]
    pub fn persistence_failed(&self, session_key: &str) -> bool {
        let failures = self
            .persistence_failures
            .lock()
            .expect("persistence failures lock poisoned");
        failures.contains(session_key)
    }

    /// Clear a recorded persistence failure.
    ///
    /// Called once a writer has successfully reloaded the session's history
    /// from the backend, which re-establishes agreement between the in-memory
    /// `Agent` and what is actually stored.
    pub fn clear_persistence_failure(&self, session_key: &str) {
        let mut failures = self
            .persistence_failures
            .lock()
            .expect("persistence failures lock poisoned");
        failures.remove(session_key);
    }
}

/// RAII guard holding a session in the finalizing state.
///
/// Using a guard rather than paired calls means every early return, `?`, and
/// panic on a completion path still releases the hold — the failure modes
/// where a stuck `finalizing` entry would wedge a session permanently.
#[derive(Debug)]
pub struct FinalizingGuard<'a> {
    lifecycle: &'a SessionLifecycle,
    session_key: String,
}

impl<'a> FinalizingGuard<'a> {
    /// Begin finalizing `session_key`, releasing on drop.
    #[must_use]
    pub fn new(lifecycle: &'a SessionLifecycle, session_key: &str) -> Self {
        lifecycle.begin_finalizing(session_key);
        Self {
            lifecycle,
            session_key: session_key.to_string(),
        }
    }
}

impl Drop for FinalizingGuard<'_> {
    fn drop(&mut self) {
        self.lifecycle.end_finalizing(&self.session_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_deleted_session_reports_unchanged_generation() {
        let lifecycle = SessionLifecycle::new();
        // The brand-new-session case: a writer captures a generation for a
        // session whose backing file does not exist yet, and must not be
        // treated as deleted when it acquires the permit.
        let captured = lifecycle.deletion_generation("brand-new");
        assert!(
            !lifecycle.deleted_since("brand-new", captured),
            "a session that was never deleted must not look deleted"
        );
    }

    #[test]
    fn deletion_invalidates_a_generation_captured_before_it() {
        let lifecycle = SessionLifecycle::new();
        let captured = lifecycle.deletion_generation("doomed");
        lifecycle.record_deletion("doomed");
        assert!(
            lifecycle.deleted_since("doomed", captured),
            "a writer queued across a delete must observe it"
        );
    }

    #[test]
    fn delete_then_recreate_still_invalidates_the_queued_writer() {
        let lifecycle = SessionLifecycle::new();
        let captured = lifecycle.deletion_generation("recycled");
        lifecycle.record_deletion("recycled");
        // The session exists again by the time the writer wakes, so an
        // existence probe would say "fine". The writer's history is stale
        // regardless, so the generation must still differ.
        assert!(
            lifecycle.deleted_since("recycled", captured),
            "delete+recreate must not look identical to never-deleted"
        );
    }

    #[test]
    fn deletion_generation_is_per_session() {
        let lifecycle = SessionLifecycle::new();
        let other = lifecycle.deletion_generation("bystander");
        lifecycle.record_deletion("doomed");
        assert!(
            !lifecycle.deleted_since("bystander", other),
            "deleting one session must not invalidate writers on another"
        );
    }

    #[test]
    fn finalizing_is_observable_until_the_guard_drops() {
        let lifecycle = SessionLifecycle::new();
        assert!(!lifecycle.is_finalizing("s"));
        {
            let _guard = FinalizingGuard::new(&lifecycle, "s");
            assert!(
                lifecycle.is_finalizing("s"),
                "a turn between stream-end and persisted must read as finalizing"
            );
        }
        assert!(
            !lifecycle.is_finalizing("s"),
            "the guard must clear the hold on drop"
        );
    }

    #[test]
    fn overlapping_finalizations_do_not_clear_each_other_early() {
        let lifecycle = SessionLifecycle::new();
        let outer = FinalizingGuard::new(&lifecycle, "s");
        {
            let _inner = FinalizingGuard::new(&lifecycle, "s");
        }
        assert!(
            lifecycle.is_finalizing("s"),
            "an inner hold releasing must not cancel the outer one"
        );
        drop(outer);
        assert!(!lifecycle.is_finalizing("s"));
    }

    #[test]
    fn end_finalizing_without_a_begin_is_inert() {
        let lifecycle = SessionLifecycle::new();
        // Defensive: a stray release must not underflow or wedge the session
        // into a permanently-finalizing state.
        lifecycle.end_finalizing("never-started");
        assert!(!lifecycle.is_finalizing("never-started"));
    }

    #[test]
    fn forget_finalizing_clears_holds_but_keeps_the_deletion_generation() {
        let lifecycle = SessionLifecycle::new();
        let captured = lifecycle.deletion_generation("gone");
        lifecycle.begin_finalizing("gone");
        lifecycle.record_deletion("gone");
        lifecycle.forget_finalizing("gone");
        assert!(!lifecycle.is_finalizing("gone"));
        assert!(
            lifecycle.deleted_since("gone", captured),
            "forgetting finalization must not erase the deletion a queued writer still needs to see"
        );
    }
}
