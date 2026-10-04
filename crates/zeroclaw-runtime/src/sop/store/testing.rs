//! Store doubles for exercising SOP persistence failure paths, shared by this
//! crate's tests and by other crates' tests through the `test-util` feature.

use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    ClaimToken, PersistedRun, ProposalRecord, ProposalStatus, RetentionPolicy, SopEventRecord,
    SopRunStore, StoreError,
};

/// Wraps a run store and fails its first terminal write (`finish_run` or
/// `finish_run_with_event`), then behaves exactly like the wrapped store.
///
/// For proving that a transient failure on the write that settles a run leaves
/// someone owning the retry, rather than a run that stays `Running` and
/// claimed forever.
pub struct FailFirstTerminalWrite<S> {
    inner: S,
    fail_next: AtomicBool,
}

impl<S> FailFirstTerminalWrite<S> {
    /// Wrap `inner`; its first terminal write will fail.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            fail_next: AtomicBool::new(true),
        }
    }

    /// Whether the injected failure has been spent.
    pub fn fired(&self) -> bool {
        !self.fail_next.load(Ordering::SeqCst)
    }

    fn take_failure(&self) -> Result<(), StoreError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(StoreError::Backend(
                "injected failure on the first terminal write".into(),
            ));
        }
        Ok(())
    }
}

impl<S: SopRunStore> SopRunStore for FailFirstTerminalWrite<S> {
    fn save_run(&self, run: &PersistedRun) -> Result<(), StoreError> {
        self.inner.save_run(run)
    }

    fn save_run_with_pending_capacity(
        &self,
        run: &PersistedRun,
        max_pending: usize,
    ) -> Result<bool, StoreError> {
        self.inner.save_run_with_pending_capacity(run, max_pending)
    }

    fn save_run_with_event(
        &self,
        run: &PersistedRun,
        ev: &SopEventRecord,
    ) -> Result<u64, StoreError> {
        self.inner.save_run_with_event(run, ev)
    }

    fn finish_run(&self, run_id: &str, terminal: &PersistedRun) -> Result<(), StoreError> {
        self.take_failure()?;
        self.inner.finish_run(run_id, terminal)
    }

    fn finish_run_with_event(
        &self,
        run_id: &str,
        terminal: &PersistedRun,
        ev: &SopEventRecord,
    ) -> Result<u64, StoreError> {
        self.take_failure()?;
        self.inner.finish_run_with_event(run_id, terminal, ev)
    }

    fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError> {
        self.inner.load_active_runs()
    }

    fn load_terminal_runs(&self, limit: usize) -> Result<Vec<PersistedRun>, StoreError> {
        self.inner.load_terminal_runs(limit)
    }

    fn load_run(&self, run_id: &str) -> Result<Option<PersistedRun>, StoreError> {
        self.inner.load_run(run_id)
    }

    fn last_terminal_completed_at(&self, sop_name: &str) -> Result<Option<String>, StoreError> {
        self.inner.last_terminal_completed_at(sop_name)
    }

    fn try_claim_run(
        &self,
        run_id: &str,
        sop_name: &str,
        per_sop_cap: usize,
        global_cap: usize,
    ) -> Result<Option<ClaimToken>, StoreError> {
        self.inner
            .try_claim_run(run_id, sop_name, per_sop_cap, global_cap)
    }

    fn renew_claim_for_restore(
        &self,
        run_id: &str,
        sop_name: &str,
    ) -> Result<ClaimToken, StoreError> {
        self.inner.renew_claim_for_restore(run_id, sop_name)
    }

    fn mark_claim_retained_after_terminal_rollback(&self, run_id: &str) -> Result<(), StoreError> {
        self.inner
            .mark_claim_retained_after_terminal_rollback(run_id)
    }

    fn has_retained_terminal_rollback_claim(&self, run_id: &str) -> Result<bool, StoreError> {
        self.inner.has_retained_terminal_rollback_claim(run_id)
    }

    fn claim_counts(&self, sop_name: &str) -> Result<(usize, usize), StoreError> {
        self.inner.claim_counts(sop_name)
    }

    fn heartbeat_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
        self.inner.heartbeat_claim(token)
    }

    fn release_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
        self.inner.release_claim(token)
    }

    fn expired_claims(&self, now_iso: &str) -> Result<Vec<ClaimToken>, StoreError> {
        self.inner.expired_claims(now_iso)
    }

    fn append_event(&self, ev: &SopEventRecord) -> Result<u64, StoreError> {
        self.inner.append_event(ev)
    }

    fn list_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
        self.inner.list_events(run_id)
    }

    fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError> {
        self.inner.save_proposal(p)
    }

    fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
        self.inner.load_proposal(id)
    }

    fn list_proposals(
        &self,
        status: Option<ProposalStatus>,
    ) -> Result<Vec<ProposalRecord>, StoreError> {
        self.inner.list_proposals(status)
    }

    fn prune(&self, policy: &RetentionPolicy) -> Result<usize, StoreError> {
        self.inner.prune(policy)
    }

    fn health_check(&self) -> bool {
        self.inner.health_check()
    }

    fn backend(&self) -> &'static str {
        self.inner.backend()
    }
}
