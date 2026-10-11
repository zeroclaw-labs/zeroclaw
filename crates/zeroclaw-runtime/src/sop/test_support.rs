//! Shared test-only `SopRunStore` doubles used by more than one test module
//! under `sop/` and `tools/`. Kept in one place so a fixture used by both
//! `engine.rs`'s own tests and `tools/sop_execute.rs`/`tools/sop_advance.rs`'s
//! doesn't get hand-copied (and drift) across files.

use super::store::{
    ClaimToken, InMemoryRunStore, PersistedRun, ProposalRecord, ProposalStatus, RetentionPolicy,
    SopEventRecord, SopRunStore, StoreError,
};

/// Wraps an in-memory store and fails every `finish_run`/`finish_run_with_event`
/// call, so a caller relying on a terminal transition actually persisting
/// (cancellation, completion, failure) can be tested against a persistence
/// failure without a real, flaky I/O fault.
///
/// Unlike `engine.rs`'s own `FailFirstFinishStore` (which recovers after one
/// failure, for tests that need the run to eventually finish), this fails
/// unconditionally: the tests that use it are about what a caller reports
/// when the terminal write never lands, not about a later retry succeeding.
pub(crate) struct AlwaysFailFinishStore {
    inner: InMemoryRunStore,
}

impl AlwaysFailFinishStore {
    pub(crate) fn new() -> Self {
        Self {
            inner: InMemoryRunStore::new(),
        }
    }
}

impl SopRunStore for AlwaysFailFinishStore {
    fn save_run(&self, run: &PersistedRun) -> Result<(), StoreError> {
        self.inner.save_run(run)
    }

    fn save_run_with_event(
        &self,
        run: &PersistedRun,
        ev: &SopEventRecord,
    ) -> Result<u64, StoreError> {
        self.inner.save_run_with_event(run, ev)
    }

    fn finish_run(&self, _run_id: &str, _terminal: &PersistedRun) -> Result<(), StoreError> {
        Err(StoreError::Backend(
            "injected terminal persistence failure".into(),
        ))
    }

    fn finish_run_with_event(
        &self,
        _run_id: &str,
        _terminal: &PersistedRun,
        _ev: &SopEventRecord,
    ) -> Result<u64, StoreError> {
        Err(StoreError::Backend(
            "injected terminal persistence failure".into(),
        ))
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

    fn append_event(&self, event: &SopEventRecord) -> Result<u64, StoreError> {
        self.inner.append_event(event)
    }

    fn list_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
        self.inner.list_events(run_id)
    }

    fn save_proposal(&self, proposal: &ProposalRecord) -> Result<(), StoreError> {
        self.inner.save_proposal(proposal)
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
        "always-fail-finish-test"
    }
}
