//! Authoritative in-process session lifecycle state for queued gateway writers.
//!
//! Queued writers (both WebSocket prompt paths and the REST message-append
//! endpoint) wait on a `session_queue` permit. While they wait, an operator
//! can delete the session, and the turn holding the permit can still be
//! finalizing. Three questions must be answered *after* the permit is acquired,
//! and none can be answered by probing the backend for existence:
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
//! * "Is this incarnation's transcript complete?" A consumable boolean lets
//!   the first queued socket hide a failure from every other writer, while a
//!   monotonic generation alone lets writers prepared after the failure
//!   certify the same partial transcript. A poisoned disposition rejects both
//!   until durable delete/recreate establishes a fresh incarnation.
//!
//! Deletion and concurrent failure generations are lifecycle facts about the
//! gateway's own bookkeeping, so they are tracked here explicitly. Durable
//! transcript completeness belongs to `SessionBackend`: this authority checks
//! that marker while holding the same lock used for mutation.
//!
//! The deletion counter is monotonic per session key. The persistence-failure
//! counter is monotonic within one incarnation and resets only after durable
//! deletion establishes the next clean incarnation. Writers capture the deletion counter
//! *before* waiting and compare *after* acquiring. Comparing generations
//! rather than reading a boolean "is deleted" flag keeps delete/recreate
//! cycles unambiguous — a session deleted and recreated while a writer queued
//! has a different generation even though it exists again at both ends of the
//! wait, and that writer's view of history is stale either way.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A session's deletion generation, captured before a writer starts waiting.
///
/// Compare with [`SessionLifecycle::deletion_generation`] after acquiring the
/// permit; any change means the session was deleted (and possibly recreated)
/// while the writer was queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletionGeneration(u64);

/// A session transcript's persistence-failure generation.
///
/// Connections capture this when their in-memory Agent is seeded. A changed
/// value means that Agent predates a partial/failed backend write and must not
/// run another prompt until it has been reloaded and the prompt rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistenceGeneration(u64);

#[derive(Debug, Default)]
struct SessionAuthority {
    deletion: u64,
    persistence_failure: u64,
}

/// Tracks per-session lifecycle authority and in-progress finalization.
///
/// An authority cell is created when a session first participates in a
/// lifecycle check or mutation. A session that has never been deleted reports
/// generation 0, which is exactly what a writer that captured its generation
/// before the session existed also sees — the "brand-new session" case that
/// must not be mistaken for deletion.
#[derive(Debug, Default)]
pub struct SessionLifecycle {
    /// Canonical per-session authority cells for incarnation and persistence
    /// disposition. Writers hold only the matching session's mutex through
    /// backend mutation and version publication, so unrelated sessions remain
    /// concurrent while same-session checks become an authority boundary.
    authorities: Mutex<HashMap<String, Arc<Mutex<SessionAuthority>>>>,
    finalizing: Mutex<HashMap<String, u64>>,
}

/// Why a queued writer cannot mutate the session it was prepared against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationRejection {
    Deleted,
    PersistenceChanged,
    PersistencePoisoned,
}

/// Completion-scoped authority to publish a session's persistence disposition.
pub struct SessionDisposition<'a> {
    authority: &'a mut SessionAuthority,
}

impl SessionDisposition<'_> {
    /// Record a failed/partial persistence attempt without releasing mutation
    /// authority. Every Agent or REST request prepared against an earlier
    /// generation will independently observe the increment.
    pub fn record_persistence_failure(&mut self) {
        self.authority.persistence_failure = self.authority.persistence_failure.saturating_add(1);
    }
}

/// Deletion-scoped authority to establish the next clean incarnation.
pub struct DeletionDisposition<'a> {
    authority: &'a mut SessionAuthority,
}

impl DeletionDisposition<'_> {
    /// Clear a poisoned transcript only after durable deletion succeeded.
    ///
    /// A failed or no-op delete must leave the poison in place: backend load
    /// cannot prove a partial transcript was repaired.
    pub fn establish_fresh_incarnation(&mut self) {
        self.authority.persistence_failure = 0;
    }
}

impl SessionLifecycle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn authority_for(&self, session_key: &str) -> Arc<Mutex<SessionAuthority>> {
        let mut authorities = self
            .authorities
            .lock()
            .expect("session authorities lock poisoned");
        Arc::clone(
            authorities
                .entry(session_key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(SessionAuthority::default()))),
        )
    }

    /// Read `session_key`'s current deletion generation.
    ///
    /// Callers capture this *before* awaiting a `session_queue` permit and
    /// re-compare after acquiring it.
    #[must_use]
    pub fn deletion_generation(&self, session_key: &str) -> DeletionGeneration {
        let authority = self.authority_for(session_key);
        let authority = authority.lock().expect("session authority lock poisoned");
        DeletionGeneration(authority.deletion)
    }

    /// Record that `session_key` was deleted, invalidating every writer that
    /// captured a generation before this call.
    pub fn record_deletion(&self, session_key: &str) {
        self.with_deletion(session_key, |_| ());
    }

    /// True when `session_key` was deleted since `captured` was read.
    #[must_use]
    pub fn deleted_since(&self, session_key: &str, captured: DeletionGeneration) -> bool {
        self.deletion_generation(session_key) != captured
    }

    /// Read the persistence-failure generation current for an Agent or REST
    /// writer snapshot.
    #[must_use]
    pub fn persistence_generation(&self, session_key: &str) -> PersistenceGeneration {
        let authority = self.authority_for(session_key);
        let authority = authority.lock().expect("session authority lock poisoned");
        PersistenceGeneration(authority.persistence_failure)
    }

    /// Capture one existing session's writer generations under the same
    /// authority used by DELETE.
    ///
    /// This closes the lookup-to-snapshot race for REST writers: a request
    /// cannot pair an existence result from the old incarnation with lifecycle
    /// generations captured after that incarnation was deleted.
    pub fn capture_existing_writer(
        &self,
        session_key: &str,
        exists: impl FnOnce() -> bool,
    ) -> Option<(DeletionGeneration, PersistenceGeneration)> {
        let authority = self.authority_for(session_key);
        let authority = authority.lock().expect("session authority lock poisoned");
        if !exists() {
            return None;
        }
        Some((
            DeletionGeneration(authority.deletion),
            PersistenceGeneration(authority.persistence_failure),
        ))
    }

    /// True when the current incarnation has a partial or failed transcript.
    ///
    /// Poison is not consumable and is not repaired by a successful load. It
    /// remains authoritative until durable deletion establishes a fresh
    /// incarnation.
    #[must_use]
    pub fn persistence_poisoned(&self, session_key: &str) -> bool {
        let authority = self.authority_for(session_key);
        let authority = authority.lock().expect("session authority lock poisoned");
        authority.persistence_failure > 0
    }

    /// Acquire mutation authority for a queued writer.
    ///
    /// Both generations are validated under the same lock the writer retains
    /// through append and version publication. DELETE and a completing turn
    /// therefore cannot land between validation and mutation.
    pub fn with_write<R>(
        &self,
        session_key: &str,
        deletion: DeletionGeneration,
        persistence: PersistenceGeneration,
        durable_incomplete: impl FnOnce() -> bool,
        mutate: impl FnOnce(&mut SessionDisposition<'_>) -> R,
    ) -> Result<R, MutationRejection> {
        let authority = self.authority_for(session_key);
        let mut authority = authority.lock().expect("session authority lock poisoned");
        if DeletionGeneration(authority.deletion) != deletion {
            return Err(MutationRejection::Deleted);
        }
        if PersistenceGeneration(authority.persistence_failure) != persistence {
            return Err(MutationRejection::PersistenceChanged);
        }
        if authority.persistence_failure > 0 || durable_incomplete() {
            return Err(MutationRejection::PersistencePoisoned);
        }
        let mut disposition = SessionDisposition {
            authority: &mut authority,
        };
        Ok(mutate(&mut disposition))
    }

    /// Run an incarnation-owned mutation on an existing session, holding the
    /// authority across both the existence decision and the mutation.
    ///
    /// A metadata writer such as rename has no queue permit to re-validate
    /// against, so it cannot capture a generation and compare it later: DELETE
    /// could land in that window and the mutation would apply to whatever was
    /// recreated under the same key. Probing and mutating under one hold
    /// removes the window instead of checking across it. Returns `None` when
    /// the session does not exist for this authority.
    pub fn with_existing_incarnation<R>(
        &self,
        session_key: &str,
        exists: impl FnOnce() -> bool,
        mutate: impl FnOnce() -> R,
    ) -> Option<R> {
        let authority = self.authority_for(session_key);
        let _authority = authority.lock().expect("session authority lock poisoned");
        if !exists() {
            return None;
        }
        Some(mutate())
    }

    /// Run an incarnation-owned mutation only while `deletion` is current.
    ///
    /// Turn state and handshake metadata use this seam so an old connection
    /// cannot recreate or mutate rows belonging to a delete/recreate cycle.
    pub fn with_incarnation<R>(
        &self,
        session_key: &str,
        deletion: DeletionGeneration,
        mutate: impl FnOnce() -> R,
    ) -> Option<R> {
        let authority = self.authority_for(session_key);
        let authority = authority.lock().expect("session authority lock poisoned");
        if DeletionGeneration(authority.deletion) != deletion {
            return None;
        }
        Some(mutate())
    }

    /// Acquire completion authority for the turn's original incarnation.
    ///
    /// Persistence failure is an outcome this completion may publish, not a
    /// reason to reject its own disposition, so only deletion is checked.
    pub fn with_completion<R>(
        &self,
        session_key: &str,
        deletion: DeletionGeneration,
        complete: impl FnOnce(&mut SessionDisposition<'_>) -> R,
    ) -> Option<R> {
        let authority = self.authority_for(session_key);
        let mut authority = authority.lock().expect("session authority lock poisoned");
        if DeletionGeneration(authority.deletion) != deletion {
            return None;
        }
        let mut disposition = SessionDisposition {
            authority: &mut authority,
        };
        Some(complete(&mut disposition))
    }

    /// Advance the deletion generation and retain exclusive lifecycle
    /// authority through `delete`.
    ///
    /// DELETE runs backend removal and epoch eviction inside this closure;
    /// writers cannot append or publish a version in the middle.
    pub fn with_deletion<R>(
        &self,
        session_key: &str,
        delete: impl FnOnce(&mut DeletionDisposition<'_>) -> R,
    ) -> R {
        let authority = self.authority_for(session_key);
        let mut authority = authority.lock().expect("session authority lock poisoned");
        authority.deletion = authority.deletion.saturating_add(1);
        let mut disposition = DeletionDisposition {
            authority: &mut authority,
        };
        delete(&mut disposition)
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
    fn incarnation_owned_mutations_cannot_cross_delete_recreate() {
        let lifecycle = SessionLifecycle::new();
        let old = lifecycle.deletion_generation("recreated");
        lifecycle.with_deletion("recreated", |disposition| {
            disposition.establish_fresh_incarnation();
        });

        assert!(
            lifecycle
                .with_incarnation("recreated", old, || panic!("old mutation ran"))
                .is_none(),
            "metadata and state prepared for the old incarnation must be rejected"
        );
        let fresh = lifecycle.deletion_generation("recreated");
        assert_eq!(
            lifecycle.with_incarnation("recreated", fresh, || "fresh"),
            Some("fresh")
        );
    }

    #[test]
    fn persistence_failure_generation_is_monotonic_and_not_consumed() {
        let lifecycle = SessionLifecycle::new();
        let session_key = "damaged";
        let deletion = lifecycle.deletion_generation(session_key);
        let socket_b = lifecycle.persistence_generation(session_key);
        let socket_c = socket_b;

        lifecycle
            .with_completion(session_key, deletion, |disposition| {
                disposition.record_persistence_failure();
            })
            .expect("session incarnation remains current");

        assert!(matches!(
            lifecycle.with_write(session_key, deletion, socket_b, || false, |_| ()),
            Err(MutationRejection::PersistenceChanged)
        ));
        assert!(matches!(
            lifecycle.with_write(session_key, deletion, socket_c, || false, |_| ()),
            Err(MutationRejection::PersistenceChanged)
        ));

        let prepared_after_failure = lifecycle.persistence_generation(session_key);
        assert!(matches!(
            lifecycle.with_write(
                session_key,
                deletion,
                prepared_after_failure,
                || false,
                |_| (),
            ),
            Err(MutationRejection::PersistencePoisoned)
        ));

        lifecycle.with_deletion(session_key, |disposition| {
            disposition.establish_fresh_incarnation();
        });
        let fresh_deletion = lifecycle.deletion_generation(session_key);
        let fresh_persistence = lifecycle.persistence_generation(session_key);
        assert!(
            lifecycle
                .with_write(
                    session_key,
                    fresh_deletion,
                    fresh_persistence,
                    || false,
                    |_| (),
                )
                .is_ok(),
            "successful delete/recreate establishes the only clean disposition"
        );
    }

    #[test]
    fn mutation_authority_is_independent_between_sessions() {
        let lifecycle = Arc::new(SessionLifecycle::new());
        let deletion_a = lifecycle.deletion_generation("a");
        let persistence_a = lifecycle.persistence_generation("a");
        let deletion_b = lifecycle.deletion_generation("b");
        let persistence_b = lifecycle.persistence_generation("b");
        let (entered_a_tx, entered_a_rx) = std::sync::mpsc::channel();
        let (release_a_tx, release_a_rx) = std::sync::mpsc::channel();

        let lifecycle_a = Arc::clone(&lifecycle);
        let writer_a = std::thread::spawn(move || {
            lifecycle_a
                .with_write(
                    "a",
                    deletion_a,
                    persistence_a,
                    || false,
                    |_| {
                        entered_a_tx.send(()).expect("test receiver remains alive");
                        release_a_rx.recv().expect("test sender remains alive");
                    },
                )
                .expect("session a remains current");
        });
        entered_a_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("session a writer acquires its authority");

        let (completed_b_tx, completed_b_rx) = std::sync::mpsc::channel();
        let lifecycle_b = Arc::clone(&lifecycle);
        let writer_b = std::thread::spawn(move || {
            lifecycle_b
                .with_write(
                    "b",
                    deletion_b,
                    persistence_b,
                    || false,
                    |_| {
                        completed_b_tx
                            .send(())
                            .expect("test receiver remains alive");
                    },
                )
                .expect("session b remains current");
        });
        completed_b_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("session b must not wait for session a's mutation authority");

        release_a_tx.send(()).expect("test receiver remains alive");
        writer_a.join().expect("session a writer does not panic");
        writer_b.join().expect("session b writer does not panic");
    }

    #[test]
    fn existing_writer_snapshot_is_atomic_with_delete() {
        let lifecycle = Arc::new(SessionLifecycle::new());
        let (lookup_entered_tx, lookup_entered_rx) = std::sync::mpsc::channel();
        let (release_lookup_tx, release_lookup_rx) = std::sync::mpsc::channel();

        let writer_lifecycle = Arc::clone(&lifecycle);
        let writer = std::thread::spawn(move || {
            writer_lifecycle
                .capture_existing_writer("rest", || {
                    lookup_entered_tx
                        .send(())
                        .expect("test receiver remains alive");
                    release_lookup_rx.recv().expect("test sender remains alive");
                    true
                })
                .expect("session existed during the authoritative snapshot")
        });
        lookup_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("writer entered backend existence lookup");

        let (deleted_tx, deleted_rx) = std::sync::mpsc::channel();
        let delete_lifecycle = Arc::clone(&lifecycle);
        let delete = std::thread::spawn(move || {
            delete_lifecycle.record_deletion("rest");
            deleted_tx.send(()).expect("test receiver remains alive");
        });
        assert!(
            deleted_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
            "DELETE must not split the existence decision from its generation snapshot"
        );

        release_lookup_tx
            .send(())
            .expect("test receiver remains alive");
        let (deletion, persistence) = writer.join().expect("writer does not panic");
        deleted_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("DELETE completes after the snapshot releases authority");
        delete.join().expect("delete does not panic");

        assert!(matches!(
            lifecycle.with_write("rest", deletion, persistence, || false, |_| ()),
            Err(MutationRejection::Deleted)
        ));
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
