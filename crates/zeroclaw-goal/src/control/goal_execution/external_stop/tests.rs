//! Exact process-local custody tests; these do not fabricate provider usage.

use super::*;
use crate::test_support::{CleanupFault, NoModelAccounting, PauseWinner, Registry};
use crate::{GoalIngressPrincipal, GoalSessionKey};

struct NoExecutionDriver(GoalSessionKey);

#[async_trait::async_trait]
impl GoalSessionDriver for NoExecutionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        &self.0
    }

    async fn bind(&self, _: &GoalIngressContext) -> Result<GoalSessionLease> {
        bail!("external custody fixture must not acquire command authority")
    }

    async fn acquire_execution(
        &self,
        _: &GoalIngressContext,
        _: &GoalExecutionScope,
        _: &GoalParentTurnCheckpoint,
    ) -> Result<Box<dyn GoalSessionExecutionLease>> {
        bail!("external custody fixture must not execute or publish")
    }
}

struct Fixture {
    registry: Arc<Registry>,
    supervisor: Arc<GoalExecutionSupervisor>,
    scope: GoalExecutionScope,
    worker: Arc<Mutex<GoalWorker>>,
    completion: watch::Sender<Option<GoalWorkerCompletion>>,
    checkpoint: GoalParentTurnCheckpoint,
}

async fn fixture(completed: bool) -> Fixture {
    let key = GoalSessionKey::zero_code("external-custody").unwrap();
    let session = key.durable_id();
    let registry = Arc::new(Registry::default());
    registry.seed(&session, TaskStatus::Running);
    let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
        GoalRuntime::new(registry.clone()).execution_engine(Arc::new(NoModelAccounting)),
    )));
    let scope = GoalExecutionScope::new("fixture-goal", &session, 1).unwrap();
    let checkpoint = GoalParentTurnCheckpoint::new();
    let (completion, observed) =
        watch::channel(completed.then_some(Ok(GoalExecutionOutcome::Fenced)));
    let worker = Arc::new(Mutex::new(GoalWorker {
        task_id: scope.task_id().into(),
        execution_epoch: scope.execution_epoch(),
        driver: Arc::new(NoExecutionDriver(key.clone())),
        ingress: GoalIngressContext::trusted(
            key,
            "fixture-agent",
            "fixture-route",
            GoalIngressPrincipal::ZeroCode {
                tui_id: "fixture-client".into(),
            },
        )
        .unwrap(),
        failure_publication: Arc::new(Mutex::new(GoalFailurePublication::default())),
        checkpoint: checkpoint.clone(),
        notice_withdrawal: GoalParentTurnCheckpoint::new(),
        completion: observed,
        lifecycle_finalization: Arc::new(Mutex::new(())),
        external_stop: Arc::new(GoalExternalStopDuty::default()),
        _handle: zeroclaw_spawn::spawn!(async {}),
    }));
    supervisor
        .workers
        .lock()
        .await
        .insert(session, worker.clone());
    Fixture {
        registry,
        supervisor,
        scope,
        worker,
        completion,
        checkpoint,
    }
}

fn assert_fault(error: &anyhow::Error, text: &str) {
    assert!(
        error.chain().any(|cause| cause
            .downcast_ref::<CleanupFault>()
            .is_some_and(|fault| fault.0 == text)),
        "{error:#}"
    );
}

fn unclassified_completion(message: &str) -> GoalWorkerFailure {
    GoalWorkerFailure {
        message: message.into(),
        accounting: None,
    }
}

async fn wait_signal(signal: &tokio::sync::Semaphore) {
    tokio::time::timeout(std::time::Duration::from_secs(1), signal.acquire())
        .await
        .expect("external-stop fixture must reach its exact latch")
        .unwrap()
        .forget();
}

#[tokio::test]
async fn complete_accounted_error_after_exact_pause_preserves_operator_pause() {
    // A fully accounted error that settles after the committed pause cannot replace it.
    let fixture = fixture(false).await;
    let (task, goal) = {
        let mut state = fixture.registry.0.lock().unwrap();
        let task = state.task.as_mut().unwrap();
        task.status = TaskStatus::Paused;
        task.execution_epoch = 2;
        let goal = state.goal.as_mut().unwrap();
        goal.pause_reason = Some(GoalPauseReason::OperatorPaused);
        goal.pause_description = Some("preserve committed pause details".into());
        (
            serde_json::to_value(state.task.as_ref().unwrap()).unwrap(),
            state.goal.clone().unwrap(),
        )
    };
    fixture
        .completion
        .send(Some(Err(unclassified_completion(
            "fixture classified operation failure",
        ))))
        .unwrap();
    let drain = fixture
        .supervisor
        .drain_lifecycle_fence(&fixture.scope)
        .await
        .unwrap();
    assert!(drain.error.is_none());
    assert!(drain.failure_notice.is_none());
    let state = fixture.registry.0.lock().unwrap();
    assert_eq!(
        serde_json::to_value(state.task.as_ref().unwrap()).unwrap(),
        task
    );
    assert_eq!(state.goal.as_ref().unwrap(), &goal);
    assert!(state.terminal_reason.is_none());
    assert_eq!(state.finish_calls, 0);
}

#[tokio::test]
async fn completion_error_preserves_exact_pause_and_extension() {
    // A success transition that merely loses its fence must not invent a terminal failure.
    let fixture = fixture(false).await;
    let (task, goal) = {
        let mut state = fixture.registry.0.lock().unwrap();
        let task = state.task.as_mut().unwrap();
        task.status = TaskStatus::Paused;
        task.execution_epoch = 2;
        let goal = state.goal.as_mut().unwrap();
        goal.pause_reason = Some(GoalPauseReason::NeedsUserInput);
        goal.pause_description = Some("preserve opaque paused question".into());
        goal.effective_token_limit = Some(4321);
        (
            serde_json::to_value(state.task.as_ref().unwrap()).unwrap(),
            state.goal.clone().unwrap(),
        )
    };
    fixture
        .completion
        .send(Some(Err(unclassified_completion(
            "fixture unclassified completion error",
        ))))
        .unwrap();
    let drain = fixture
        .supervisor
        .drain_lifecycle_fence(&fixture.scope)
        .await
        .unwrap();
    assert!(drain.error.is_none());
    assert!(drain.failure_notice.is_none());
    let state = fixture.registry.0.lock().unwrap();
    assert_eq!(
        serde_json::to_value(state.task.as_ref().unwrap()).unwrap(),
        task
    );
    assert_eq!(state.goal.as_ref().unwrap(), &goal);
    assert!(state.terminal_reason.is_none());
    assert_eq!(state.finish_calls, 0);
}

#[tokio::test]
async fn completion_error_after_pause_keeps_unknown_accounting_terminal() {
    // Accounting uncertainty remains terminal even after a committed pause.
    let fixture = fixture(false).await;
    {
        let mut state = fixture.registry.0.lock().unwrap();
        let task = state.task.as_mut().unwrap();
        task.status = TaskStatus::Paused;
        task.execution_epoch = 2;
        state.goal.as_mut().unwrap().accounting_state = GoalAccountingState::OutcomeUnknown;
    }
    fixture
        .completion
        .send(Some(Err(unclassified_completion(
            "fixture classified operation failure",
        ))))
        .unwrap();
    let drain = fixture
        .supervisor
        .drain_lifecycle_fence(&fixture.scope)
        .await
        .unwrap();
    assert!(drain.error.is_none());
    {
        let state = fixture.registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.execution_epoch, 3);
        assert_eq!(
            state.terminal_reason.as_deref(),
            Some("accounting_outcome_unknown")
        );
        assert_eq!(
            state.goal.as_ref().unwrap().accounting_state,
            GoalAccountingState::OutcomeUnknown
        );
        assert_eq!(state.finish_calls, 1);
    }
    drain
        .failure_notice
        .expect("accounting failure owes its once-only notice")
        .withdraw()
        .await;
}

#[tokio::test]
async fn typed_worker_accounting_failure_overrides_exact_pause_without_pending_marker() {
    let fixture = fixture(false).await;
    {
        let mut state = fixture.registry.0.lock().unwrap();
        let task = state.task.as_mut().unwrap();
        task.status = TaskStatus::Paused;
        task.execution_epoch = 2;
        let goal = state.goal.as_ref().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
    }
    fixture
        .completion
        .send(Some(Err(GoalWorkerFailure {
            message: "fixture worker returned a terminal error".into(),
            accounting: Some((GoalAccountingState::Complete, true)),
        })))
        .unwrap();
    let drain = fixture
        .supervisor
        .drain_lifecycle_fence(&fixture.scope)
        .await
        .unwrap();
    assert!(drain.error.is_none());
    {
        let state = fixture.registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.execution_epoch, 3);
        assert_eq!(
            state.terminal_reason.as_deref(),
            Some("accounting_outcome_unknown")
        );
        assert_eq!(state.finish_calls, 1);
    }
    drain
        .failure_notice
        .expect("typed accounting uncertainty owes its once-only notice")
        .withdraw()
        .await;
}

#[tokio::test]
async fn completion_error_preserves_newer_pause_and_cancelled_winners() {
    // An old-worker error cannot mutate a newer pause or an exact terminal winner.
    for (status, epoch) in [(TaskStatus::Paused, 3), (TaskStatus::Cancelled, 2)] {
        let fixture = fixture(false).await;
        let (task, goal) = {
            let mut state = fixture.registry.0.lock().unwrap();
            let task = state.task.as_mut().unwrap();
            task.status = status;
            task.execution_epoch = epoch;
            let goal = state.goal.as_mut().unwrap();
            goal.pause_reason = Some(GoalPauseReason::NeedsUserInput);
            goal.pause_description = Some("preserve winning opaque state".into());
            goal.effective_token_limit = Some(4321);
            (
                serde_json::to_value(state.task.as_ref().unwrap()).unwrap(),
                state.goal.clone().unwrap(),
            )
        };
        fixture
            .completion
            .send(Some(Err(unclassified_completion(
                "fixture classified operation failure",
            ))))
            .unwrap();
        let drain = fixture
            .supervisor
            .drain_lifecycle_fence(&fixture.scope)
            .await
            .unwrap();
        assert!(drain.error.is_none());
        assert!(drain.failure_notice.is_none());
        let state = fixture.registry.0.lock().unwrap();
        assert_eq!(
            serde_json::to_value(state.task.as_ref().unwrap()).unwrap(),
            task
        );
        assert_eq!(state.goal.as_ref().unwrap(), &goal);
        assert!(state.terminal_reason.is_none());
        assert_eq!(state.finish_calls, 0);
    }
}

#[tokio::test]
async fn completion_error_still_fails_exact_running_worker() {
    // The existing Running-plus-error fallback remains terminal.
    let fixture = fixture(false).await;
    fixture
        .completion
        .send(Some(Err(unclassified_completion(
            "fixture unclassified completion error",
        ))))
        .unwrap();
    let drain = fixture
        .supervisor
        .drain_lifecycle_fence(&fixture.scope)
        .await
        .unwrap();
    assert!(drain.error.is_none());
    {
        let state = fixture.registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.execution_epoch, 2);
        assert_eq!(state.terminal_reason.as_deref(), Some("executor_failed"));
        assert_eq!(state.finish_calls, 1);
    }
    drain
        .failure_notice
        .expect("Running worker failure still owes its notice")
        .withdraw()
        .await;
}

#[tokio::test]
async fn public_drain_preserves_original_completion_text() {
    // Public drain preserves the original error text after exact Running finalization.
    let original =
        "fixture original completion error\ncaused by: source chain detail\n  retained tail";
    let fixture = fixture(false).await;
    // Publication is already withdrawn; this fixture must not acquire an output lease.
    {
        let worker = fixture.worker.lock().await;
        worker.notice_withdrawal.close();
        worker.failure_publication.lock().await.notice_withdrawn = true;
    }
    fixture
        .completion
        .send(Some(Err(unclassified_completion(original))))
        .unwrap();
    let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
    assert_eq!(error.to_string(), original);
    assert_eq!(format!("{error:#}"), original);
    {
        let state = fixture.registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.execution_epoch, 2);
        assert_eq!(state.terminal_reason.as_deref(), Some("executor_failed"));
        assert_eq!(state.finish_calls, 1);
    }
    assert!(fixture.supervisor.try_retire().await);
}

#[tokio::test]
async fn capture_is_effect_free_and_completed_idle_custody_is_untouched() {
    let fixture = fixture(false).await;
    let baseline = fixture.registry.0.lock().unwrap().session_lookups;
    let target = fixture
        .supervisor
        .capture_external_cancellation(fixture.scope.session_id())
        .await
        .unwrap()
        .unwrap();
    drop(target);
    assert_eq!(fixture.registry.0.lock().unwrap().session_lookups, baseline);
    drop(fixture.checkpoint.register_control().unwrap());
    assert!(!fixture.worker.lock().await.external_stop.active());

    fixture
        .completion
        .send(Some(Ok(GoalExecutionOutcome::Fenced)))
        .unwrap();
    fixture
        .registry
        .0
        .lock()
        .unwrap()
        .task
        .as_mut()
        .unwrap()
        .status = TaskStatus::Paused;
    let boundary = GoalParentTurnCheckpoint::new();
    fixture
        .supervisor
        .pending_boundaries
        .lock()
        .await
        .insert(fixture.scope.session_id().into(), vec![boundary.clone()]);
    let publication = fixture.worker.lock().await.failure_publication.clone();
    let mut held = publication.lock().await;
    held.core_error =
        GoalCoreErrorPublication::Parked(anyhow::Error::new(CleanupFault("idle owned original")));
    assert!(
        fixture
            .supervisor
            .capture_external_cancellation(fixture.scope.session_id())
            .await
            .unwrap()
            .is_none()
    );
    drop(boundary.register_control().unwrap());
    drop(fixture.checkpoint.register_control().unwrap());
    assert!(matches!(
        held.core_error,
        GoalCoreErrorPublication::Parked(_)
    ));
    assert!(!held.notice_withdrawn && !held.notice_published);
    assert!(Arc::ptr_eq(
        fixture
            .supervisor
            .workers
            .lock()
            .await
            .get(fixture.scope.session_id())
            .unwrap(),
        &fixture.worker
    ));
    assert_eq!(fixture.registry.0.lock().unwrap().pause_calls, 0);
    assert!(!fixture.supervisor.try_retire().await);
}

#[tokio::test]
async fn read_or_pause_fault_still_drains_and_preserves_primary_plus_distinct_secondary() {
    for read_fault in [false, true] {
        let fixture = fixture(false).await;
        let primary = if read_fault {
            "distinct external fence read failure"
        } else {
            "fixture cleanup pause unavailable"
        };
        {
            let mut state = fixture.registry.0.lock().unwrap();
            state.fail_pause = !read_fault;
            if read_fault {
                state.fail_lookup_at = Some(state.session_lookups + 1);
                state.lookup_failure_text = Some(primary);
            }
        }
        let fence = fixture
            .supervisor
            .capture_external_cancellation(fixture.scope.session_id())
            .await
            .unwrap()
            .unwrap()
            .fence()
            .await;
        assert!(fixture.checkpoint.register_control().is_err());
        {
            let mut state = fixture.registry.0.lock().unwrap();
            assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
            state.fail_lookup_at = Some(state.session_lookups + 1);
            state.lookup_failure_text = Some("distinct external drain lookup failure");
        }
        fixture
            .completion
            .send(Some(Ok(GoalExecutionOutcome::Fenced)))
            .unwrap();
        let outcome = fence.drain().await;
        assert!(outcome.operation_drained);
        assert_eq!(
            outcome.scopes[0].disposition,
            GoalExternalStopDisposition::Unresolved
        );
        assert!(outcome.failure_notice.is_none());
        let error = outcome.error.unwrap();
        assert_fault(&error, primary);
        let chain = format!("{error:#}");
        assert!(chain.contains("distinct external drain lookup failure"));
        assert!(!fixture.supervisor.try_retire().await);

        fixture.registry.0.lock().unwrap().fail_pause = false;
        let mut retry = fixture
            .supervisor
            .drain_lifecycle_fence(&fixture.scope)
            .await
            .unwrap();
        assert_fault(&retry.error.take().unwrap(), primary);
        assert!(retry.failure_notice.is_none());
        let next = fixture
            .supervisor
            .drain_lifecycle_fence(&fixture.scope)
            .await
            .unwrap();
        assert!(next.error.is_none());
        assert_eq!(
            fixture
                .registry
                .0
                .lock()
                .unwrap()
                .task
                .as_ref()
                .unwrap()
                .status,
            TaskStatus::Paused
        );
        assert!(fixture.supervisor.try_retire().await);
    }
}

#[tokio::test]
async fn aborting_unlocked_drain_keeps_exact_worker_and_known_pause_error() {
    let fixture = fixture(false).await;
    fixture.registry.0.lock().unwrap().fail_pause = true;
    let fence = fixture
        .supervisor
        .capture_external_cancellation(fixture.scope.session_id())
        .await
        .unwrap()
        .unwrap()
        .fence()
        .await;
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    {
        let mut state = fixture.registry.0.lock().unwrap();
        state.lookup_wait_at = Some(state.session_lookups + 1);
        state.lookup_wait = Some(Arc::new(tokio::sync::Semaphore::new(0)));
        state.lookup_entered = Some(entered.clone());
    }
    let task = zeroclaw_spawn::spawn!(fence.drain());
    wait_signal(&entered).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let duty = fixture.worker.lock().await.external_stop.clone();
    assert!(duty.active());
    assert_fault(&duty.error().unwrap(), "fixture cleanup pause unavailable");
    assert!(fixture.checkpoint.register_control().is_err());
    assert!(!fixture.supervisor.try_retire().await);
    fixture.registry.0.lock().unwrap().fail_pause = false;
    fixture
        .completion
        .send(Some(Ok(GoalExecutionOutcome::Fenced)))
        .unwrap();
    let mut retry = fixture
        .supervisor
        .drain_lifecycle_fence(&fixture.scope)
        .await
        .unwrap();
    assert_fault(
        &retry.error.take().unwrap(),
        "fixture cleanup pause unavailable",
    );
    assert!(!duty.active());
    assert!(fixture.supervisor.try_retire().await);
}

#[tokio::test]
async fn missing_worker_completion_never_certifies_operation_drain_or_retirement() {
    let fixture = fixture(false).await;
    let fence = fixture
        .supervisor
        .capture_external_cancellation(fixture.scope.session_id())
        .await
        .unwrap()
        .unwrap()
        .fence()
        .await;
    drop(fixture.completion);
    let outcome = fence.drain().await;
    assert!(!outcome.operation_drained);
    assert!(outcome.error.is_some());
    if let Some(delivery) = outcome.failure_notice {
        delivery.withdraw().await;
    }
    assert!(fixture.worker.lock().await.external_stop.active());
    assert!(fixture.supervisor.owns_scope(&fixture.scope).await);
    assert!(!fixture.supervisor.try_retire().await);
}

#[tokio::test]
async fn held_launch_pause_fault_blocks_handoff_until_its_holder_reconciles() {
    let fixture = fixture(true).await;
    fixture.supervisor.workers.lock().await.clear();
    let mut launch = GoalLaunchGuard::new(fixture.supervisor.launches.clone(), None, None);
    launch.arm(fixture.scope.clone()).unwrap();
    fixture.registry.0.lock().unwrap().fail_pause = true;
    let fence = fixture
        .supervisor
        .capture_external_cancellation(fixture.scope.session_id())
        .await
        .unwrap()
        .unwrap()
        .fence()
        .await;
    assert!(
        fixture
            .supervisor
            .launches
            .launch_permitted(&fixture.scope)
            .unwrap_err()
            .is::<GoalExternalStopClosed>()
    );
    let outcome = fence.drain().await;
    assert!(outcome.operation_drained);
    assert_fault(&outcome.error.unwrap(), "fixture cleanup pause unavailable");
    assert!(!fixture.supervisor.try_retire().await);
    assert_eq!(fixture.registry.0.lock().unwrap().pause_calls, 1);
    fixture.registry.0.lock().unwrap().fail_pause = false;
    assert_fault(
        &launch.reconcile_held().await.unwrap_err(),
        "fixture cleanup pause unavailable",
    );
    launch.settled();
    assert_eq!(fixture.registry.0.lock().unwrap().pause_calls, 2);
    assert!(fixture.supervisor.try_retire().await);
}

#[tokio::test]
async fn held_launch_holder_lookup_error_preserves_earlier_external_pause_fault() {
    let fixture = fixture(true).await;
    fixture.supervisor.workers.lock().await.clear();
    let mut launch = GoalLaunchGuard::new(fixture.supervisor.launches.clone(), None, None);
    launch.arm(fixture.scope.clone()).unwrap();
    fixture.registry.0.lock().unwrap().fail_pause = true;
    let fence = fixture
        .supervisor
        .capture_external_cancellation(fixture.scope.session_id())
        .await
        .unwrap()
        .unwrap()
        .fence()
        .await;
    let outcome = fence.drain().await;
    assert!(outcome.operation_drained);
    assert_fault(&outcome.error.unwrap(), "fixture cleanup pause unavailable");
    {
        let mut state = fixture.registry.0.lock().unwrap();
        state.fail_lookup_at = Some(state.session_lookups + 1);
        state.lookup_failure_text = Some("holder lookup unavailable");
    }
    let error = launch.reconcile_held().await.unwrap_err();
    assert_fault(&error, "fixture cleanup pause unavailable");
    let layer = error.downcast_ref::<GoalLifecycleSecondaryError>().unwrap();
    assert_fault(&layer.secondary, "holder lookup unavailable");
    assert_fault(&layer.primary, "fixture cleanup pause unavailable");
    assert!(
        layer
            .primary
            .downcast_ref::<GoalLifecycleSecondaryError>()
            .is_none()
    );
    for text in [
        "fixture cleanup pause unavailable",
        "holder lookup unavailable",
    ] {
        assert_eq!(format!("{error:#}").matches(text).count(), 1);
    }
    assert!(
        fixture
            .supervisor
            .launches
            .launch_permitted(&fixture.scope)
            .unwrap_err()
            .is::<GoalExternalStopClosed>()
    );
    assert!(!fixture.supervisor.try_retire().await);
    assert_eq!(fixture.registry.0.lock().unwrap().pause_calls, 1);
    {
        let mut state = fixture.registry.0.lock().unwrap();
        state.fail_pause = false;
        state.fail_lookup_at = None;
        state.lookup_failure_text = None;
    }
    let error = launch.reconcile_held().await.unwrap_err();
    assert_fault(&error, "fixture cleanup pause unavailable");
    let layer = error.downcast_ref::<GoalLifecycleSecondaryError>().unwrap();
    assert_fault(&layer.secondary, "holder lookup unavailable");
    assert_fault(&layer.primary, "fixture cleanup pause unavailable");
    assert!(
        layer
            .primary
            .downcast_ref::<GoalLifecycleSecondaryError>()
            .is_none()
    );
    for text in [
        "fixture cleanup pause unavailable",
        "holder lookup unavailable",
    ] {
        assert_eq!(format!("{error:#}").matches(text).count(), 1);
    }
    assert_eq!(fixture.registry.0.lock().unwrap().pause_calls, 2);
    launch.settled();
    assert!(fixture.supervisor.try_retire().await);
}

#[tokio::test]
async fn every_captured_worker_launch_and_pending_boundary_closes_before_lifecycle_io() {
    let fixture = fixture(false).await;
    let next = GoalExecutionScope::new("fixture-goal", fixture.scope.session_id(), 3).unwrap();
    let mut launch = GoalLaunchGuard::new(fixture.supervisor.launches.clone(), None, None);
    launch.arm(next.clone()).unwrap();
    fixture
        .registry
        .0
        .lock()
        .unwrap()
        .task
        .as_mut()
        .unwrap()
        .execution_epoch = 3;
    let boundary = GoalParentTurnCheckpoint::new();
    fixture
        .supervisor
        .pending_boundaries
        .lock()
        .await
        .insert(fixture.scope.session_id().into(), vec![boundary.clone()]);
    let target = fixture
        .supervisor
        .capture_external_cancellation(fixture.scope.session_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.scopes.len(), 2);
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    {
        let mut state = fixture.registry.0.lock().unwrap();
        state.lookup_wait_at = Some(state.session_lookups + 1);
        state.lookup_entered = Some(entered.clone());
        state.lookup_wait = Some(release.clone());
    }
    let task = zeroclaw_spawn::spawn!(target.fence());
    wait_signal(&entered).await;
    assert!(fixture.checkpoint.register_control().is_err());
    assert!(boundary.register_control().is_err());
    assert!(
        fixture
            .supervisor
            .launches
            .launch_permitted(&next)
            .unwrap_err()
            .is::<GoalExternalStopClosed>()
    );
    assert_eq!(fixture.registry.0.lock().unwrap().pause_calls, 0);
    release.add_permits(1);
    let fence = task.await.unwrap();
    fixture
        .completion
        .send(Some(Ok(GoalExecutionOutcome::Fenced)))
        .unwrap();
    let outcome = fence.drain().await;
    assert!(outcome.operation_drained && outcome.error.is_none());
    assert_eq!(fixture.registry.0.lock().unwrap().pause_calls, 1);
    launch.reconcile_held().await.unwrap();
    launch.settled();
}

#[tokio::test]
async fn missing_transition_requires_an_actual_canonical_winner_without_spinning() {
    for absent in [false, true] {
        let fixture = fixture(true).await;
        fixture.supervisor.workers.lock().await.clear();
        {
            let mut state = fixture.registry.0.lock().unwrap();
            state.pause_result = Some(GoalTransitionResult::Missing);
            if absent {
                state.pause_winner = Some(PauseWinner::Delete);
            }
        }
        let result = fixture
            .supervisor
            .reconcile_external_scope(&fixture.scope, &GoalExternalStopDuty::default(), true)
            .await;
        if absent {
            assert_eq!(result.unwrap(), GoalExternalStopDisposition::Absent);
        } else {
            assert!(format!("{:#}", result.unwrap_err()).contains("made no progress"));
        }
        let state = fixture.registry.0.lock().unwrap();
        assert_eq!(state.session_lookups, 2);
        assert_eq!(state.pause_calls, 1);
        assert_eq!(state.finish_calls, 0);
    }
}

#[tokio::test]
async fn canonical_winners_are_preserved_and_running_successor_requires_real_custody() {
    for status in [
        TaskStatus::Paused,
        TaskStatus::Completed,
        TaskStatus::Cancelled,
        TaskStatus::Failed,
    ] {
        let fixture = fixture(true).await;
        fixture.supervisor.workers.lock().await.clear();
        let pause = GoalPauseState {
            reason: GoalPauseReason::NeedsUserInput,
            description: Some("preserve opaque question".into()),
            blockers: Vec::new(),
        };
        {
            let mut state = fixture.registry.0.lock().unwrap();
            state.pause_result = Some(GoalTransitionResult::Stale);
            state.pause_winner = Some(if status == TaskStatus::Paused {
                PauseWinner::Pause(pause)
            } else {
                PauseWinner::Finish(status)
            });
            state.goal.as_mut().unwrap().effective_token_limit = Some(4321);
        }
        let duty = GoalExternalStopDuty::default();
        let result = fixture
            .supervisor
            .reconcile_external_scope(&fixture.scope, &duty, true)
            .await
            .unwrap();
        assert_eq!(
            result,
            if status == TaskStatus::Paused {
                GoalExternalStopDisposition::Paused
            } else {
                GoalExternalStopDisposition::Terminal(status)
            }
        );
        let state = fixture.registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().status, status);
        assert_eq!(
            state.goal.as_ref().unwrap().effective_token_limit,
            Some(4321)
        );
        assert_eq!(state.pause_calls, 1);
        assert_eq!(state.finish_calls, 0);
        if status == TaskStatus::Paused {
            assert_eq!(
                state.goal.as_ref().unwrap().pause_description.as_deref(),
                Some("preserve opaque question")
            );
        }
    }
    for owned in [false, true] {
        let fixture = fixture(true).await;
        fixture.supervisor.workers.lock().await.clear();
        fixture
            .registry
            .0
            .lock()
            .unwrap()
            .task
            .as_mut()
            .unwrap()
            .execution_epoch = 3;
        let mut successor = GoalLaunchGuard::new(fixture.supervisor.launches.clone(), None, None);
        if owned {
            successor
                .arm(
                    GoalExecutionScope::new("fixture-goal", fixture.scope.session_id(), 3).unwrap(),
                )
                .unwrap();
        }
        let result = fixture
            .supervisor
            .reconcile_external_scope(&fixture.scope, &GoalExternalStopDuty::default(), true)
            .await;
        if owned {
            assert_eq!(result.unwrap(), GoalExternalStopDisposition::SuccessorOwned);
        } else {
            assert!(
                format!("{:#}", result.unwrap_err()).contains("Running without its exact owner")
            );
        }
        assert_eq!(fixture.registry.0.lock().unwrap().pause_calls, 0);
        assert_eq!(
            fixture
                .registry
                .0
                .lock()
                .unwrap()
                .task
                .as_ref()
                .unwrap()
                .execution_epoch,
            3
        );
        fixture
            .registry
            .0
            .lock()
            .unwrap()
            .task
            .as_mut()
            .unwrap()
            .status = TaskStatus::Completed;
        successor.reconcile_held().await.unwrap();
        successor.settled();
    }
}

#[test]
fn dropping_external_fence_without_runtime_retains_the_existing_launch_duty() {
    let registry = Arc::new(Registry::default());
    registry.seed("external-runtime-free", TaskStatus::Running);
    registry.0.lock().unwrap().fail_pause = true;
    let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
        GoalRuntime::new(registry.clone()).execution_engine(Arc::new(NoModelAccounting)),
    )));
    let scope = GoalExecutionScope::new("fixture-goal", "external-runtime-free", 1).unwrap();
    let mut launch = GoalLaunchGuard::new(supervisor.launches.clone(), None, None);
    launch.arm(scope.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let fence = runtime.block_on(async {
        supervisor
            .capture_external_cancellation(scope.session_id())
            .await
            .unwrap()
            .unwrap()
            .fence()
            .await
    });
    drop(runtime);
    drop(fence);
    assert!(
        supervisor
            .launches
            .launch_permitted(&scope)
            .unwrap_err()
            .is::<GoalExternalStopClosed>()
    );
    // The pre-existing launch guard's own fallback retains its one scheduling
    // fault. External-stop Drop adds neither a service nor a detached attempt.
    drop(launch);
    registry.0.lock().unwrap().fail_pause = false;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let error = runtime
        .block_on(supervisor.launches.retry_scope(&scope, false))
        .unwrap_err();
    assert_fault(&error, "fixture cleanup pause unavailable");
    assert!(format!("{error:#}").contains("scheduling unavailable"));
    assert!(!supervisor.launches.owns(&scope));
    assert_eq!(registry.0.lock().unwrap().pause_calls, 2);
}
