//! Coordinates the existing Goal and session owners across a session effect.
//! This adapter never retries the session mutation. Only the named effect
//! receipt can permit cleanup of the task captured before it.

pub(super) mod durable;

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
};

use anyhow::{Context as _, Result, bail, ensure};
use zeroclaw_api::task::TaskRecord;
use zeroclaw_goal::control::GoalSessionKey;
use zeroclaw_infra::{
    acp_session_store::AcpSessionStore,
    session_disposition::{
        SessionDispositionBackend, SessionDispositionEffect, SessionDispositionObservation,
        SessionDispositionWitness,
    },
    session_sqlite::SqliteSessionBackend,
    session_store::SessionStore,
};

use crate::control_plane::{
    SqliteTaskStore,
    authority::is_authoritative_owner,
    task_store_sqlite::session_disposition::{
        DispositionResult, SessionDisposition, SessionDispositionKind, SessionDispositionOrigin,
        SessionDispositionPhase,
    },
};

use super::context::RpcContext;

// Process-local effect custody across RPC context replacement. This set owns
// no transition or binding: it only distinguishes a dispatched effect from
// a row that can be inspected by the same process after that effect returns.
static IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

struct EffectCustody(String);

impl EffectCustody {
    fn claim(operation_id: &str) -> Option<Self> {
        IN_FLIGHT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(operation_id.to_owned())
            .then(|| Self(operation_id.to_owned()))
    }
}

impl Drop for EffectCustody {
    fn drop(&mut self) {
        IN_FLIGHT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

/// Ephemeral access to the existing task store and configured session
/// directory. The path is never taken from a request or from a receipt.
#[derive(Clone)]
pub(super) struct DispositionHost {
    pub(super) store: Arc<SqliteTaskStore>,
    boot_id: String,
    data_dir: PathBuf,
}

/// Keep this with the tracked effect, not the RPC response future.
pub(super) struct OwnedDisposition {
    host: DispositionHost,
    pub(super) record: SessionDisposition,
    _custody: EffectCustody,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolvedEffect {
    NotCommitted,
    Committed,
}

impl DispositionHost {
    pub(super) fn for_context(context: &RpcContext) -> Result<Option<Self>> {
        let Some(handle) = context.goal_runtime.current_control_plane() else {
            return Ok(None);
        };
        Ok(Some(Self {
            store: handle.session_dispositions()?,
            boot_id: handle.boot_id,
            data_dir: context.config.read().data_dir.clone(),
        }))
    }

    pub(super) fn pending(&self, raw_session_id: &str) -> Result<Option<SessionDisposition>> {
        let Ok(key) = GoalSessionKey::zero_code(raw_session_id) else {
            // Ordinary RPC sessions need not opt into Goal identity rules.
            // Their file-backed aliases still cannot overwrite a backend
            // tombstone, which is checked against the physical key.
            return Ok(None);
        };
        self.store.session_disposition(&key.durable_id())
    }

    pub(super) fn prepare(
        self,
        raw_session_id: &str,
        task: &TaskRecord,
        kind: SessionDispositionKind,
        origin: SessionDispositionOrigin,
        live_generation: Option<u64>,
    ) -> Result<OwnedDisposition> {
        let record = SessionDisposition::new(
            GoalSessionKey::zero_code(raw_session_id)?.durable_id(),
            task,
            kind,
            origin,
            self.boot_id.clone(),
            live_generation,
        );
        // Register before the SQL insert; no observer can find a prepared
        // row while the effect is live but not represented here.
        let custody = EffectCustody::claim(&record.operation_id)
            .context("session disposition operation identity already in use")?;
        ensure!(
            self.store.prepare_session_disposition(&record)? == DispositionResult::Applied,
            "session disposition target or custody changed"
        );
        Ok(OwnedDisposition {
            host: self,
            record,
            _custody: custody,
        })
    }

    pub(super) fn is_local_owner(&self, record: &SessionDisposition) -> bool {
        record.owner_pid == std::process::id() && record.owner_boot_id == self.boot_id
    }

    /// Claims recovery custody without authorizing any native session action.
    /// A live or indeterminate foreign owner must settle its own operation.
    pub(super) fn claim_recovery(
        self,
        record: SessionDisposition,
    ) -> Result<Option<OwnedDisposition>> {
        if !self.is_local_owner(&record)
            && !is_authoritative_owner(record.owner_pid, &record.owner_boot_id)
        {
            return Ok(None);
        }
        let Some(custody) = EffectCustody::claim(&record.operation_id) else {
            return Ok(None);
        };
        let Some(current) = self.store.session_disposition(&record.session_id)? else {
            return Ok(None);
        };
        if current.operation_id != record.operation_id {
            return Ok(None);
        }
        Ok(Some(OwnedDisposition {
            host: self,
            record: current,
            _custody: custody,
        }))
    }

    /// Claim a committed durable effect whose exact old local generation may
    /// still be resident. Call only after ordinary admission for a removal,
    /// off the async executor. This is recovery custody, not authority to
    /// remove the live session: the caller must independently check the
    /// method and reauthorize against the exact generation at native removal.
    /// An abandoned live preparation for the exact retry is aborted without
    /// replaying it, so the caller can perform its own authorized removal.
    pub(super) fn claim_committed_live_removal(
        &self,
        raw_session_id: &str,
        generation: u64,
        kind: SessionDispositionKind,
    ) -> Result<Option<OwnedDisposition>> {
        let Some(record) = self.pending(raw_session_id)? else {
            return Ok(None);
        };
        let retry_prepared_live = |record: &SessionDisposition| {
            matches!(record.origin, SessionDispositionOrigin::Live)
                && record.phase == SessionDispositionPhase::Prepared
                && record.kind == kind
        };
        let matches_live_removal = |record: &SessionDisposition| {
            self.is_local_owner(record)
                && record.live_generation == Some(generation)
                && (retry_prepared_live(record)
                    || (matches!(record.origin, SessionDispositionOrigin::Durable { .. })
                        && matches!(
                            record.kind,
                            SessionDispositionKind::Delete | SessionDispositionKind::Kill
                        )))
        };
        ensure!(
            matches_live_removal(&record),
            "pending session disposition does not match the local removal"
        );
        let mut operation = self
            .clone()
            .claim_recovery(record)?
            .context("pending session disposition changed or is still in flight")?;
        ensure!(
            matches_live_removal(&operation.record),
            "claimed session disposition does not match the local removal"
        );
        // A prepared live effect has not committed. Durable effects still
        // require exact original-store evidence; ambiguity retains the fence.
        let is_prepared_live_retry = retry_prepared_live(&operation.record);
        match operation.resolve_effect()? {
            ResolvedEffect::NotCommitted => Ok(None),
            ResolvedEffect::Committed => {
                ensure!(
                    !is_prepared_live_retry,
                    "prepared live removal became committed"
                );
                Ok(Some(operation))
            }
        }
    }
}

impl OwnedDisposition {
    pub(super) fn witness(&self) -> Result<Option<SessionDispositionWitness>> {
        let SessionDispositionOrigin::Durable {
            backend_key,
            identity,
        } = &self.record.origin
        else {
            return Ok(None);
        };
        let effect = match self.record.kind {
            SessionDispositionKind::Delete => SessionDispositionEffect::Delete,
            SessionDispositionKind::Kill => SessionDispositionEffect::Kill,
            _ => bail!("invalid durable session disposition effect"),
        };
        Ok(Some(SessionDispositionWitness {
            operation_id: self.record.operation_id.clone(),
            effect,
            backend_key: backend_key.clone(),
            identity: identity.clone(),
        }))
    }

    pub(super) fn store(&self) -> &SqliteTaskStore {
        &self.host.store
    }

    /// A tagged refusal proves this attempt never reached publication. Only
    /// this operation's custody holder may use that proof instead of observing.
    pub(super) fn resolve_applied(
        &mut self,
        applied: &Result<durable::DurableRemovalOutcome>,
    ) -> Result<ResolvedEffect> {
        if !applied.as_ref().is_err_and(durable::is_not_attempted) {
            return self.resolve_effect();
        }
        let current = self
            .store()
            .session_disposition(&self.record.session_id)?
            .context("session disposition disappeared before its effect was resolved")?;
        ensure!(
            current.operation_id == self.record.operation_id
                && current.phase == SessionDispositionPhase::Prepared,
            "session disposition is no longer the exact prepared operation"
        );
        let result = self.store().abort_session_disposition(
            &self.record,
            std::process::id(),
            &self.host.boot_id,
        )?;
        ensure!(
            result != DispositionResult::Conflict,
            "session disposition could not preserve its original target"
        );
        Ok(ResolvedEffect::NotCommitted)
    }

    /// Resolve a completed dispatch/previous owner's operation. A genuine
    /// rejection leaves the original Goal untouched. A positive receipt must
    /// reach the canonical committed phase before acknowledgement.
    pub(super) fn resolve_effect(&mut self) -> Result<ResolvedEffect> {
        let current = self
            .store()
            .session_disposition(&self.record.session_id)?
            .context("session disposition disappeared before its effect was resolved")?;
        ensure!(
            current.operation_id == self.record.operation_id,
            "session disposition was superseded"
        );
        self.record = current;
        if self.record.phase == SessionDispositionPhase::SessionCommitted {
            return Ok(ResolvedEffect::Committed);
        }

        let observation = match self.witness()? {
            None => SessionDispositionObservation::NotCommitted,
            Some(witness) => self.observe_original(&witness)?,
        };
        let owner = (std::process::id(), self.host.boot_id.as_str());
        match observation {
            SessionDispositionObservation::NotCommitted => {
                let result =
                    self.store()
                        .abort_session_disposition(&self.record, owner.0, owner.1)?;
                ensure!(
                    result != DispositionResult::Conflict,
                    "session disposition could not preserve its original target"
                );
                Ok(ResolvedEffect::NotCommitted)
            }
            SessionDispositionObservation::Committed => {
                let result =
                    self.store()
                        .confirm_session_disposition(&self.record, owner.0, owner.1)?;
                ensure!(
                    result != DispositionResult::Conflict,
                    "session disposition could not confirm its exact effect"
                );
                self.record.phase = SessionDispositionPhase::SessionCommitted;
                Ok(ResolvedEffect::Committed)
            }
            SessionDispositionObservation::Indeterminate => {
                bail!("session disposition effect is indeterminate");
            }
        }
    }

    /// Call only after the old live incarnation is no longer admissible and
    /// native storage has committed. Failure retains the exact intent.
    pub(super) fn finish_committed(self) -> Result<()> {
        ensure!(
            self.record.phase == SessionDispositionPhase::SessionCommitted,
            "session disposition effect was not committed"
        );
        if let Some(witness) = self.witness()? {
            self.acknowledge_original(&witness)?;
        }
        let result = self.store().finalize_session_disposition(
            &self.record,
            std::process::id(),
            &self.host.boot_id,
        )?;
        ensure!(
            result != DispositionResult::Conflict,
            "session disposition could not clean up its exact Goal"
        );
        Ok(())
    }

    fn observe_original(
        &self,
        witness: &SessionDispositionWitness,
    ) -> Result<SessionDispositionObservation> {
        let dir = &self.host.data_dir;
        Ok(match witness.identity.backend {
            SessionDispositionBackend::ChatSqlite => {
                SqliteSessionBackend::observe_session_disposition_at(dir, witness)?
            }
            SessionDispositionBackend::ChatJsonl => {
                SessionStore::observe_original_session_disposition(dir, witness)?
            }
            SessionDispositionBackend::AcpSqlite => {
                AcpSessionStore::observe_session_disposition_at(dir, witness)?
            }
        })
    }

    fn acknowledge_original(&self, witness: &SessionDispositionWitness) -> Result<()> {
        let dir = &self.host.data_dir;
        match witness.identity.backend {
            SessionDispositionBackend::ChatSqlite => {
                SqliteSessionBackend::acknowledge_session_disposition_at(dir, witness)?;
            }
            SessionDispositionBackend::ChatJsonl => {
                SessionStore::acknowledge_original_session_disposition(dir, witness)?;
            }
            SessionDispositionBackend::AcpSqlite => {
                AcpSessionStore::acknowledge_session_disposition_at(dir, witness)?;
            }
        }
        Ok(())
    }
}

/// Inspect and reconcile an exact pending binding before a new session can
/// adopt it. The caller must apply ordinary session/transport authority first.
/// This does not acquire the actor or command lock, including when the caller
/// already holds an actor permit.
pub(super) async fn reconcile_session(context: &RpcContext, raw_session_id: &str) -> Result<bool> {
    let Some(host) = DispositionHost::for_context(context)? else {
        return Ok(true);
    };
    let lookup = host.clone();
    let session_id = raw_session_id.to_owned();
    let record = tokio::task::spawn_blocking(move || lookup.pending(&session_id))
        .await
        .context("join session disposition lookup")??;
    let Some(record) = record else {
        return Ok(true);
    };
    reconcile_record(context, host, raw_session_id, record).await
}

/// Retry outstanding exact cleanups before starting session ingress.
/// Indeterminate or live-owner records stay fenced by the per-key check.
pub(in crate::rpc) async fn reconcile_startup(context: &RpcContext) -> Result<usize> {
    let Some(host) = DispositionHost::for_context(context)? else {
        return Ok(0);
    };
    let store = Arc::clone(&host.store);
    let records = tokio::task::spawn_blocking(move || store.list_session_dispositions())
        .await
        .context("join session disposition listing")??;
    let mut unresolved = 0;
    for record in records {
        let key = record
            .session_id
            .strip_prefix("rpc_")
            .context("invalid durable RPC session identity")?
            .to_owned();
        if !matches!(
            reconcile_record(context, host.clone(), &key, record).await,
            Ok(true)
        ) {
            unresolved += 1;
        }
    }
    Ok(unresolved)
}

async fn reconcile_record(
    context: &RpcContext,
    host: DispositionHost,
    raw_session_id: &str,
    record: SessionDisposition,
) -> Result<bool> {
    let claim = host.clone();
    let operation = tokio::task::spawn_blocking(move || {
        let Some(mut operation) = claim.claim_recovery(record)? else {
            return Ok(None);
        };
        let resolved = operation.resolve_effect()?;
        Ok::<_, anyhow::Error>(Some((operation, resolved)))
    })
    .await
    .context("join session disposition recovery")??;
    let Some((operation, resolved)) = operation else {
        return Ok(false);
    };
    if resolved == ResolvedEffect::NotCommitted {
        return Ok(true);
    }
    if host.is_local_owner(&operation.record)
        && let Some(generation) = operation.record.live_generation
        && context.sessions.get_generation(raw_session_id).await == Some(generation)
    {
        // A durable session removal may have committed before a later
        // authorization denied live removal. Keep this exact old generation
        // quarantined, and never remove it using receipt authority alone.
        return Ok(false);
    }
    tokio::task::spawn_blocking(move || operation.finish_committed())
        .await
        .context("join session disposition finalization")??;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::goal_task::{
        GoalPauseReason, GoalPauseState, GoalTaskRecord, GoalTaskRegistry, GoalTransitionResult,
    };
    use zeroclaw_api::task::{TaskKind, TaskStatus};
    use zeroclaw_infra::session_backend::{SessionBackend, SessionEffectGuard};

    #[cfg(unix)]
    struct JsonlRecoveryFixture {
        context: Arc<RpcContext>,
        durable: durable::DurableRemoval,
        host: DispositionHost,
        task: TaskRecord,
        transcript: PathBuf,
        _dir: tempfile::TempDir,
    }

    #[cfg(unix)]
    impl JsonlRecoveryFixture {
        const RAW_ID: &str = "jsonl-recovery";

        async fn new() -> Self {
            use zeroclaw_api::model_provider::ChatMessage;
            use zeroclaw_infra::session_queue::SessionActorQueue;

            let dir = tempfile::tempdir().unwrap();
            let config = zeroclaw_config::schema::Config {
                data_dir: dir.path().to_path_buf(),
                ..Default::default()
            };
            let backend = Arc::new(SessionStore::new(dir.path()).unwrap());
            let queue = Arc::new(SessionActorQueue::new(4, 10, 60));
            let sessions = Arc::new(crate::rpc::session::SessionStore::new(16, queue));
            let context =
                RpcContext::for_persistence_tests(config, sessions, Some(backend.clone()), None);
            let registry = super::super::dispatch::connection_test_support::install_paused_goal_control_fixture(
                &context, dir.path(), Self::RAW_ID,
            )
            .await;
            let host = DispositionHost::for_context(&context).unwrap().unwrap();
            let key = GoalSessionKey::zero_code(Self::RAW_ID)
                .unwrap()
                .durable_id();
            backend
                .append(&key, &ChatMessage::user("original transcript"))
                .unwrap();
            let task = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            let transcript = dir.path().join("sessions").join(format!("{key}.jsonl"));
            Self {
                _dir: dir,
                context,
                durable: durable::DurableRemoval::ChatDelete { backend, key },
                host,
                task,
                transcript,
            }
        }

        fn prepare_without_custody(&self, boot_id: String) -> SessionDisposition {
            let record = SessionDisposition::new(
                self.task.session_id.as_ref().unwrap().clone(),
                &self.task,
                SessionDispositionKind::Delete,
                self.durable.origin().unwrap().unwrap(),
                boot_id,
                None,
            );
            assert_eq!(
                self.host
                    .store
                    .prepare_session_disposition(&record)
                    .unwrap(),
                DispositionResult::Applied
            );
            record
        }

        fn prepare(&self) -> OwnedDisposition {
            self.host
                .clone()
                .prepare(
                    Self::RAW_ID,
                    &self.task,
                    SessionDispositionKind::Delete,
                    self.durable.origin().unwrap().unwrap(),
                    None,
                )
                .unwrap()
        }

        async fn reconcile(&self) -> Result<bool> {
            reconcile_session(&self.context, Self::RAW_ID).await
        }

        async fn assert_retained_goal(&self) {
            let retained = self
                .host
                .store
                .current_goal_for_session(self.task.session_id.as_deref().unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.id, self.task.id);
            assert_eq!(retained.execution_epoch, self.task.execution_epoch);
            assert_eq!(retained.status, self.task.status);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn jsonl_recovery_aborts_only_a_strictly_readable_transcript() {
        let fixture = JsonlRecoveryFixture::new().await;
        fixture.prepare_without_custody(fixture.host.boot_id.clone());
        let valid = std::fs::read(&fixture.transcript).unwrap();
        assert!(fixture.reconcile().await.unwrap());
        assert_eq!(std::fs::read(&fixture.transcript).unwrap(), valid);
        assert!(
            fixture
                .host
                .store
                .list_session_dispositions()
                .unwrap()
                .is_empty()
        );
        fixture.assert_retained_goal().await;

        let expected = fixture.prepare_without_custody(fixture.host.boot_id.clone());
        let corrupt = b"corrupt ordinary JSONL\n";
        std::fs::write(&fixture.transcript, corrupt).unwrap();
        assert!(fixture.reconcile().await.is_err());
        assert_eq!(std::fs::read(&fixture.transcript).unwrap(), corrupt);
        assert_eq!(
            fixture.host.store.list_session_dispositions().unwrap(),
            vec![expected]
        );
        fixture.assert_retained_goal().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn jsonl_recovery_fences_a_committed_delete_with_a_damaged_marker() {
        let fixture = JsonlRecoveryFixture::new().await;
        let operation = fixture.prepare();
        assert_eq!(
            fixture
                .durable
                .apply(&operation.witness().unwrap().unwrap(), None, &|_| Ok(
                    Box::new(())
                ))
                .unwrap(),
            durable::DurableRemovalOutcome::Changed
        );
        let expected = operation.record.clone();
        drop(operation);
        let mut marker = std::fs::read(&fixture.transcript).unwrap();
        assert_eq!(marker[0], 0);
        marker[0] = 1;
        std::fs::write(&fixture.transcript, &marker).unwrap();
        assert!(fixture.reconcile().await.is_err());
        assert_eq!(
            fixture.host.store.list_session_dispositions().unwrap(),
            vec![expected]
        );
        assert_eq!(std::fs::read(&fixture.transcript).unwrap(), marker);
        fixture.assert_retained_goal().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn jsonl_recovery_aborts_a_tagged_refusal_after_preparation() {
        let fixture = JsonlRecoveryFixture::new().await;
        let origin = fixture.durable.origin().unwrap().unwrap();
        fixture.durable.validate_before_prepare(&origin).unwrap();
        let mut operation = fixture.prepare();
        let corrupt = b"corrupt ordinary JSONL\n";
        std::fs::write(&fixture.transcript, corrupt).unwrap();
        let witness = operation.witness().unwrap().unwrap();
        let applied = fixture.durable.apply(&witness, None, &|_| Ok(Box::new(())));
        assert!(durable::is_not_attempted(applied.as_ref().unwrap_err()));
        assert!(matches!(
            operation.resolve_applied(&applied).unwrap(),
            ResolvedEffect::NotCommitted
        ));
        assert!(
            fixture
                .host
                .store
                .list_session_dispositions()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .host
                .store
                .abort_session_disposition(
                    &operation.record,
                    std::process::id(),
                    &fixture.host.boot_id,
                )
                .unwrap(),
            DispositionResult::AlreadySettled
        );
        assert_eq!(std::fs::read(&fixture.transcript).unwrap(), corrupt);
        fixture.assert_retained_goal().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn jsonl_recovery_does_not_observe_an_intent_owned_by_a_live_foreign_boot() {
        let fixture = JsonlRecoveryFixture::new().await;
        let expected = fixture.prepare_without_custody(fixture.host.boot_id.clone());
        let corrupt = b"corrupt ordinary JSONL\n";
        std::fs::write(&fixture.transcript, corrupt).unwrap();
        let config = fixture.context.config.snapshot();
        let mut foreign = crate::control_plane::ControlPlaneHandle::open(&config.data_dir).unwrap();
        foreign.boot_id = "other-boot-fixture".into();
        let context = RpcContext::for_persistence_tests(
            config,
            Arc::clone(&fixture.context.sessions),
            None,
            None,
        );
        context.goal_runtime.install_test_control_plane(foreign);
        // Reading this corrupt transcript would return Err. The owner check
        // must instead report unresolved without entering observation.
        assert!(
            !reconcile_session(&context, JsonlRecoveryFixture::RAW_ID)
                .await
                .unwrap()
        );
        assert_eq!(
            fixture.host.store.list_session_dispositions().unwrap(),
            vec![expected]
        );
        assert_eq!(std::fs::read(&fixture.transcript).unwrap(), corrupt);
        fixture.assert_retained_goal().await;
    }

    #[tokio::test]
    async fn durable_live_removal_claim_requires_exact_local_custody_and_positive_commit() {
        const RAW_ID: &str = "late-removal";
        const BOOT: &str = "late-removal-test";
        const GENERATION: u64 = 7;
        let dir = tempfile::tempdir().unwrap();
        let host = DispositionHost {
            store: Arc::new(SqliteTaskStore::new(dir.path()).unwrap()),
            boot_id: BOOT.into(),
            data_dir: dir.path().to_path_buf(),
        };
        let backend = SqliteSessionBackend::new(dir.path()).unwrap();
        backend.set_session_principal(RAW_ID, "user:alice").unwrap();
        let durable_id = GoalSessionKey::zero_code(RAW_ID).unwrap().durable_id();
        let task = TaskRecord {
            id: "old-goal".into(),
            kind: TaskKind::Goal,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid: std::process::id(),
            owner_boot_id: BOOT.into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: Some(durable_id.clone()),
            execution_epoch: 1,
            started_at: "2026-06-18T00:00:00Z".into(),
            finished_at: None,
        };
        let goal = GoalTaskRecord {
            task_id: task.id.clone(),
            objective: "fixture objective".into(),
            effective_token_limit: Some(100),
            ..GoalTaskRecord::default()
        };
        assert_eq!(
            host.store
                .create_or_replace_session_goal(task, goal)
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        assert_eq!(
            host.store
                .pause_session_goal(
                    "old-goal",
                    &durable_id,
                    1,
                    GoalPauseState {
                        reason: GoalPauseReason::NeedsUserInput,
                        description: Some("fixture clarification".into()),
                        blockers: Vec::new(),
                    },
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        let task = host
            .store
            .current_goal_for_session(&durable_id)
            .await
            .unwrap()
            .unwrap();

        let origin = SessionDispositionOrigin::Durable {
            backend_key: RAW_ID.into(),
            identity: backend.session_disposition_identity().unwrap(),
        };
        let prepare = |origin| {
            host.clone()
                .prepare(
                    RAW_ID,
                    &task,
                    SessionDispositionKind::Delete,
                    origin,
                    Some(GENERATION),
                )
                .unwrap()
        };
        let claim_delete = |host: &DispositionHost, generation| {
            host.claim_committed_live_removal(RAW_ID, generation, SessionDispositionKind::Delete)
        };
        let foreign = DispositionHost {
            boot_id: "different-boot".into(),
            ..host.clone()
        };

        assert!(claim_delete(&host, GENERATION).unwrap().is_none());
        let rejected = prepare(origin.clone());
        let rejected_id = rejected.record.operation_id.clone();
        assert!(claim_delete(&host, GENERATION).is_err());
        drop(rejected);
        let unavailable = DispositionHost {
            data_dir: dir.path().join("unavailable"),
            ..host.clone()
        };
        assert!(claim_delete(&unavailable, GENERATION).is_err());
        assert_eq!(
            host.pending(RAW_ID).unwrap().unwrap().operation_id,
            rejected_id
        );
        assert!(claim_delete(&host, GENERATION).unwrap().is_none());
        assert!(backend.session_exists(RAW_ID));
        assert!(host.pending(RAW_ID).unwrap().is_none());

        for kind in [
            SessionDispositionKind::Close,
            SessionDispositionKind::Kill,
            SessionDispositionKind::Delete,
        ] {
            let prepared = host
                .clone()
                .prepare(
                    RAW_ID,
                    &task,
                    kind,
                    SessionDispositionOrigin::Live,
                    Some(GENERATION),
                )
                .unwrap();
            let record = prepared.record.clone();
            let claim = |host: &DispositionHost, generation, kind| {
                host.claim_committed_live_removal(RAW_ID, generation, kind)
            };
            assert!(claim(&host, GENERATION, kind).is_err());
            drop(prepared);
            assert!(claim(&host, GENERATION + 1, kind).is_err());
            assert!(claim(&foreign, GENERATION, kind).is_err());
            let wrong_kind = if kind == SessionDispositionKind::Close {
                SessionDispositionKind::Kill
            } else {
                SessionDispositionKind::Close
            };
            assert!(claim(&host, GENERATION, wrong_kind).is_err());
            assert_eq!(host.pending(RAW_ID).unwrap(), Some(record));
            assert!(claim(&host, GENERATION, kind).unwrap().is_none());
            assert!(host.pending(RAW_ID).unwrap().is_none());
            assert!(backend.session_exists(RAW_ID));
            let retained = host
                .store
                .current_goal_for_session(&durable_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.id, task.id);
            assert_eq!(retained.execution_epoch, task.execution_epoch);
            assert_eq!(retained.status, TaskStatus::Paused);
        }

        let committed = prepare(origin);
        assert!(
            backend
                .delete_session_with_disposition(
                    &committed.witness().unwrap().unwrap(),
                    Some("user:alice"),
                    &|owner: Option<&str>| -> std::io::Result<Box<dyn SessionEffectGuard>> {
                        assert_eq!(owner, Some("user:alice"));
                        Ok(Box::new(()))
                    },
                )
                .unwrap()
        );
        let operation_id = committed.record.operation_id.clone();
        drop(committed);

        assert!(claim_delete(&host, GENERATION + 1).is_err());
        assert!(claim_delete(&foreign, GENERATION).is_err());
        assert_eq!(
            host.pending(RAW_ID).unwrap().unwrap().phase,
            SessionDispositionPhase::Prepared
        );
        for _ in 0..2 {
            let recovery = claim_delete(&host, GENERATION).unwrap().unwrap();
            assert_eq!(recovery.record.operation_id, operation_id);
            assert_eq!(
                recovery.record.phase,
                SessionDispositionPhase::SessionCommitted
            );
            assert!(claim_delete(&host, GENERATION).is_err());
        }
        // Custody and even exact positive evidence cannot clean the old Goal
        // without the caller's independent native-generation removal step.
        let retained = host
            .store
            .current_goal_for_session(&durable_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.id, task.id);
        assert_eq!(retained.execution_epoch, task.execution_epoch);
    }

    #[tokio::test]
    async fn prepared_live_intent_recovery_retains_resumable_original_goal() {
        const RAW_ID: &str = "prepared-live-recovery";
        const BOOT: &str = "prepared-live-recovery-test";

        let dir = tempfile::tempdir().unwrap();
        let host = DispositionHost {
            store: Arc::new(SqliteTaskStore::new(dir.path()).unwrap()),
            boot_id: BOOT.into(),
            data_dir: dir.path().to_path_buf(),
        };
        let durable_id = GoalSessionKey::zero_code(RAW_ID).unwrap().durable_id();
        let task = TaskRecord {
            id: "resumable-goal".into(),
            kind: TaskKind::Goal,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid: std::process::id(),
            owner_boot_id: BOOT.into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            session_id: Some(durable_id.clone()),
            execution_epoch: 1,
            started_at: "2026-06-18T00:00:00Z".into(),
            finished_at: None,
        };
        let goal = GoalTaskRecord {
            task_id: task.id.clone(),
            objective: "recover after a prepared live effect".into(),
            effective_token_limit: Some(100),
            ..GoalTaskRecord::default()
        };
        assert_eq!(
            host.store
                .create_or_replace_session_goal(task, goal)
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        assert_eq!(
            host.store
                .pause_session_goal(
                    "resumable-goal",
                    &durable_id,
                    1,
                    GoalPauseState {
                        reason: GoalPauseReason::NeedsUserInput,
                        description: Some("recoverable after transport loss".into()),
                        blockers: Vec::new(),
                    },
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        let original = host
            .store
            .current_goal_for_session(&durable_id)
            .await
            .unwrap()
            .unwrap();

        // Dropping the owner after preparation models a process crash before
        // the live replacement was published; startup recovery must abort the
        // uncommitted effect rather than consuming the resumable Goal.
        let prepared = host
            .clone()
            .prepare(
                RAW_ID,
                &original,
                SessionDispositionKind::Replace,
                SessionDispositionOrigin::Live,
                Some(9),
            )
            .unwrap();
        drop(prepared);

        let recovered_host = DispositionHost {
            store: Arc::clone(&host.store),
            boot_id: BOOT.into(),
            data_dir: dir.path().to_path_buf(),
        };
        let record = recovered_host.pending(RAW_ID).unwrap().unwrap();
        let mut recovered = recovered_host.claim_recovery(record).unwrap().unwrap();
        assert!(matches!(
            recovered.resolve_effect().unwrap(),
            ResolvedEffect::NotCommitted
        ));
        assert!(host.pending(RAW_ID).unwrap().is_none());

        let retained = host
            .store
            .current_goal_for_session(&durable_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.id, original.id);
        assert_eq!(retained.execution_epoch, original.execution_epoch);
        assert_eq!(retained.status, TaskStatus::Paused);
        let retained_extension = host
            .store
            .get_goal_task(&original.id)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            retained_extension.accounting_state,
            zeroclaw_goal::GoalAccountingState::Complete
        ));
        assert!(retained_extension.pending_call_id.is_none());
        assert!(retained_extension.pending_call_epoch.is_none());
    }
}
