//! VT86 exercises the real executor checkpoint with scripted, non-provider work.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{OwnedMutexGuard, Semaphore};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_goal::{
    GoalAccountingState, GoalExecutionNotice, GoalOperationScope, GoalParentTurn,
    GoalParentTurnResult, GoalVerifierResult, GoalVerifierTurn,
    accounting::{
        GoalAccountingFailure, GoalOperationAccounting, GoalOperationAccountingFactory,
        GoalOperationRequest, GoalOperationSettlement,
    },
};

struct Script {
    entered: Semaphore,
    parent_calls: AtomicUsize,
    release: Semaphore,
    published: AtomicUsize,
    verified: AtomicUsize,
    notices: AtomicUsize,
    hold_verifier: AtomicBool,
    continue_verifier: AtomicBool,
    verifier_entered: Semaphore,
    verifier_release: Semaphore,
    fail_launch_bind: AtomicBool,
    fail_notice: AtomicBool,
    fail_notice_once: AtomicBool,
    fail_parent: AtomicBool,
    parent_accounting_error: std::sync::Mutex<Option<(GoalAccountingState, bool, bool)>>,
    verifier_accounting_error: std::sync::Mutex<Option<(GoalAccountingState, bool, bool)>>,
    reject_execution_incarnation: AtomicBool,
    publication_entered: Semaphore,
    publication_gate: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    acquisitions: AtomicUsize,
    events: std::sync::Mutex<Vec<FailureEvent>>,
}

#[derive(Debug, PartialEq)]
enum FailureEvent {
    Original(String),
    Notice(GoalExecutionNotice),
}

impl Default for Script {
    fn default() -> Self {
        Self {
            entered: Semaphore::new(0),
            parent_calls: AtomicUsize::new(0),
            release: Semaphore::new(0),
            published: AtomicUsize::new(0),
            verified: AtomicUsize::new(0),
            notices: AtomicUsize::new(0),
            hold_verifier: AtomicBool::new(false),
            continue_verifier: AtomicBool::new(false),
            verifier_entered: Semaphore::new(0),
            verifier_release: Semaphore::new(0),
            fail_launch_bind: AtomicBool::new(false),
            fail_notice: AtomicBool::new(false),
            fail_notice_once: AtomicBool::new(false),
            fail_parent: AtomicBool::new(false),
            parent_accounting_error: std::sync::Mutex::new(None),
            verifier_accounting_error: std::sync::Mutex::new(None),
            reject_execution_incarnation: AtomicBool::new(false),
            publication_entered: Semaphore::new(0),
            publication_gate: std::sync::Mutex::new(None),
            acquisitions: AtomicUsize::new(0),
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
}

/// There are no physical provider attempts in this script. Accounting admission
/// or settlement would therefore be a fixture error, not a fabricated usage pass.
struct NoPhysicalAttempts;

#[async_trait]
impl GoalOperationAccounting for NoPhysicalAttempts {
    async fn admit(&self, _request: GoalOperationRequest) -> Result<()> {
        bail!("script unexpectedly admitted a physical provider attempt")
    }

    async fn settle(&self, _settlement: GoalOperationSettlement) -> Result<()> {
        bail!("script unexpectedly settled a physical provider attempt")
    }

    async fn pricing_complete(&self) -> Result<bool> {
        Ok(true)
    }
}

impl GoalOperationAccountingFactory for NoPhysicalAttempts {
    fn for_scope(&self, _scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        Arc::new(Self)
    }
}

struct WorkerDriver {
    ordinary: Arc<SerializedBoundaryDriver>,
    script: Arc<Script>,
}

#[async_trait]
impl GoalSessionDriver for WorkerDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.ordinary.session_key()
    }

    async fn bind(&self, ingress: &GoalIngressContext) -> Result<GoalSessionLease> {
        if self.script.fail_launch_bind.swap(false, Ordering::SeqCst) {
            return Err(anyhow::Error::new(support::CleanupFault(
                "fixture launch rebind unavailable",
            )));
        }
        self.ordinary.bind(ingress).await
    }

    async fn try_turn_boundary(
        &self,
        ingress: &GoalIngressContext,
    ) -> Result<Option<GoalTurnBoundary>> {
        self.ordinary.try_turn_boundary(ingress).await
    }

    async fn await_turn_boundary(
        &self,
        ingress: &GoalIngressContext,
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> Result<GoalTurnBoundary> {
        self.ordinary.await_turn_boundary(ingress, checkpoint).await
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> Result<Box<dyn GoalSessionExecutionLease>> {
        if self
            .script
            .reject_execution_incarnation
            .load(Ordering::SeqCst)
        {
            bail!("fixture session incarnation was replaced");
        }
        let gate = self.script.publication_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            self.script.publication_entered.add_permits(1);
            tokio::select! {
                biased;
                () = checkpoint.wait_closed() => bail!("script publication withdrawn"),
                result = gate => result?,
            }
        }
        let guard = tokio::select! {
            biased;
            () = checkpoint.wait_closed() => bail!("script execution admission closed"),
            guard = self.ordinary.boundary.clone().lock_owned() => guard,
        };
        self.script.acquisitions.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(WorkerLease {
            key: self.session_key().clone(),
            script: self.script.clone(),
            _guard: guard,
        }))
    }
}

struct WorkerLease {
    key: GoalSessionKey,
    script: Arc<Script>,
    _guard: OwnedMutexGuard<()>,
}

#[async_trait]
impl GoalSessionExecutionLease for WorkerLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.key
    }

    async fn canonical_history(
        &mut self,
        _input: &zeroclaw_goal::control::GoalParentTurnInput,
    ) -> Result<Vec<ChatMessage>> {
        Ok(vec![])
    }

    async fn run_parent_turn(
        &mut self,
        _scope: &GoalOperationScope,
        _turn: GoalParentTurn,
    ) -> Result<GoalParentTurnResult> {
        self.script.parent_calls.fetch_add(1, Ordering::SeqCst);
        self.script.entered.add_permits(1);
        self.script.release.acquire().await?.forget();
        if let Some((state, pending, fake)) = *self.script.parent_accounting_error.lock().unwrap() {
            let failure = if pending {
                GoalAccountingFailure::incomplete(state, pending)
            } else {
                GoalAccountingFailure::settled(state, &["fixture cause".into()])
            };
            return Err(if fake {
                anyhow::Error::msg(failure.to_string())
            } else {
                anyhow::Error::new(failure).context("fixture parent accounting")
            });
        }
        if self.script.fail_parent.load(Ordering::SeqCst) {
            bail!("fixture parent failed after its lifecycle fence");
        }
        let candidate = "The bounded fixture is complete.";
        GoalParentTurnResult::from_completed_agent_turn(
            candidate.into(),
            vec![ChatMessage::assistant(candidate)],
        )
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> Result<()> {
        self.script.published.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn run_verifier(
        &mut self,
        _scope: &GoalOperationScope,
        _turn: GoalVerifierTurn,
    ) -> Result<GoalVerifierResult> {
        self.script.verified.fetch_add(1, Ordering::SeqCst);
        if self.script.hold_verifier.load(Ordering::SeqCst) {
            self.script.verifier_entered.add_permits(1);
            self.script.verifier_release.acquire().await?.forget();
        }
        if let Some((state, pending, fake)) = *self.script.verifier_accounting_error.lock().unwrap()
        {
            let failure = if pending {
                GoalAccountingFailure::incomplete(state, pending)
            } else {
                GoalAccountingFailure::settled(state, &["fixture cause".into()])
            };
            return Err(if fake {
                anyhow::Error::msg(failure.to_string())
            } else {
                anyhow::Error::new(failure).context("fixture verifier accounting")
            });
        }
        Ok(GoalVerifierResult {
            response: if self.script.continue_verifier.load(Ordering::SeqCst) {
                r#"{"decision":"continue","reason":"More work remains.","blockers":[]}"#
            } else {
                r#"{"decision":"complete","reason":"The declared criterion is satisfied.","blockers":[]}"#
            }.into(),
            attribution: None,
        })
    }

    async fn publish_goal_notice(&mut self, notice: GoalExecutionNotice) -> Result<()> {
        self.script.notices.fetch_add(1, Ordering::SeqCst);
        if self.script.fail_notice.load(Ordering::SeqCst)
            || self.script.fail_notice_once.swap(false, Ordering::SeqCst)
        {
            bail!("fixture notice delivery unavailable");
        }
        self.script
            .events
            .lock()
            .unwrap()
            .push(FailureEvent::Notice(notice));
        Ok(())
    }

    async fn publish_goal_core_error(&mut self, error: &anyhow::Error) -> Result<()> {
        self.script
            .events
            .lock()
            .unwrap()
            .push(FailureEvent::Original(format!("{error:#}")));
        Ok(())
    }
}

async fn running_worker() -> (
    Arc<WorkerDriver>,
    GoalIngressContext,
    Arc<Registry>,
    Arc<GoalExecutionSupervisor>,
) {
    running_worker_on(None).await
}

/// Use the real command and boundary leases without launching a parent turn.
/// A failed admission audit must publish through this same execution driver.
fn unlaunched_worker_fixture() -> (
    Arc<WorkerDriver>,
    GoalIngressContext,
    Arc<Registry>,
    Arc<GoalExecutionSupervisor>,
) {
    let (ordinary, ingress) = serialized_fixture();
    let driver = Arc::new(WorkerDriver {
        ordinary,
        script: Arc::new(Script::default()),
    });
    let registry = Arc::new(Registry::default());
    let runtime = GoalRuntime::new(registry.clone());
    let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
        runtime.execution_engine(Arc::new(NoPhysicalAttempts)),
    )));
    (driver, ingress, registry, supervisor)
}

/// Reach the audit fault only after the queued control owns its real boundary.
async fn deferred_audit_fixture() -> (
    Arc<WorkerDriver>,
    GoalIngressContext,
    Arc<Registry>,
    Arc<GoalExecutionSupervisor>,
    zeroclaw_goal::GoalDeferredReady,
) {
    let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
    let ordinary = driver.ordinary.boundary.clone().lock_owned().await;
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress.clone(),
                driver.clone(),
                launch_command(false),
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    drop(ordinary);
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(driver.ordinary.boundary.try_lock().is_err());
    registry.0.lock().unwrap().fail_settlement = true;
    (driver, ingress, registry, supervisor, ready)
}

#[tokio::test]
async fn deferred_audit_failure_does_not_self_wait_on_held_boundary() {
    let (driver, _, registry, supervisor, ready) = deferred_audit_fixture().await;
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        ready.commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        ))),
    )
    .await;
    registry.0.lock().unwrap().fail_settlement = false;
    let error = result
        .expect("audit-failure publication must release the held boundary")
        .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(
        *driver.script.events.lock().unwrap(),
        [FailureEvent::Notice(GoalExecutionNotice::Failed(
            zeroclaw_goal::GoalFailureReason::ExecutorStartFailed
        ))]
    );
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
        assert_eq!(
            state.terminal_reason.as_deref(),
            Some("executor_start_failed")
        );
        assert_eq!(state.transitions, ["start", "finish"]);
    }
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert!(driver.ordinary.boundary.try_lock().is_ok());
    assert!(matches!(
        wait_for_settlement(&registry).await,
        IntentState::Settled(_)
    ));
    assert!(retire_without_waiting(&supervisor).await);
}

#[tokio::test]
async fn unlaunched_failure_rebind_competing_start_waits_for_notice() {
    let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
    let GoalControlAdmission::Direct(mut submission) = supervisor
        .admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            launch_command(false),
        )
        .await
        .unwrap()
    else {
        panic!("idle Start must apply directly");
    };
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().id.clone();
    driver.script.fail_launch_bind.store(true, Ordering::SeqCst);
    let release = hold_publication(&driver.script);
    let failure = {
        let supervisor = supervisor.clone();
        zeroclaw_spawn::spawn!(async move {
            supervisor
                .launch_after_response(
                    &source(&LiveConfig::new(goal_config_with(
                        Some(1000),
                        None,
                        "openai.default",
                        true,
                    ))),
                    &mut submission,
                )
                .await
        })
    };
    await_publication(&driver.script).await;
    let admission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            launch_command(false),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let deferred = match admission {
        GoalControlAdmission::Queued { response, deferred } => {
            assert_eq!(response, GoalResponse::Queued(GoalQueuedControl::Start));
            deferred
        }
        GoalControlAdmission::Direct(submission) => {
            // Clean up the owned task before reporting the planned pre-fix RED.
            drop(submission);
            failure.abort();
            let _ = failure.await;
            drop(release);
            panic!("a Start must not overtake an unlaunched failure notice");
        }
    };
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().id, predecessor);
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
        assert_eq!(
            state.terminal_reason.as_deref(),
            Some("executor_start_failed")
        );
    }
    let successor = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
    release.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(1), failure)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(
        *driver.script.events.lock().unwrap(),
        [FailureEvent::Notice(GoalExecutionNotice::Failed(
            zeroclaw_goal::GoalFailureReason::ExecutorStartFailed
        ))]
    );
    let ready = tokio::time::timeout(Duration::from_secs(1), successor)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    let committed = tokio::time::timeout(
        Duration::from_secs(1),
        ready.commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        ))),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(committed.response(), GoalResponse::Started(_)));
    assert_ne!(
        registry.0.lock().unwrap().task.as_ref().unwrap().id,
        predecessor
    );
    drop(committed);
}

#[tokio::test]
async fn deferred_audit_failure_releases_command_and_boundary_before_notice() {
    let (driver, ingress, registry, supervisor, ready) = deferred_audit_fixture().await;
    let release = hold_publication(&driver.script);
    let committing = zeroclaw_spawn::spawn!(async move {
        ready
            .commit(&settings(&goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            )))
            .await
    });
    await_publication(&driver.script).await;
    let authority_free =
        driver.ordinary.command.try_lock().is_ok() && driver.ordinary.boundary.try_lock().is_ok();
    if !authority_free {
        // Release the fixture gate and consume its task before the red assertion.
        drop(release);
        tokio::time::timeout(Duration::from_secs(1), committing)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        registry.0.lock().unwrap().fail_settlement = false;
        assert!(
            authority_free,
            "audit-failure publication must not own command authority or the ordinary boundary"
        );
        return;
    }
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
        assert_eq!(
            state.terminal_reason.as_deref(),
            Some("executor_start_failed")
        );
    }
    let status = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(status, GoalControlAdmission::Direct(_)));
    drop(status);
    release.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(1), committing)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    registry.0.lock().unwrap().fail_settlement = false;
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert!(driver.ordinary.boundary.try_lock().is_ok());
}

#[tokio::test]
async fn deferred_audit_failure_notice_delivery_failure_is_still_reported() {
    let (driver, _, registry, _, ready) = deferred_audit_fixture().await;
    driver.script.fail_notice.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        ready.commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        ))),
    )
    .await;
    registry.0.lock().unwrap().fail_settlement = false;
    let error = result.unwrap().unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert!(driver.script.events.lock().unwrap().is_empty());
    let state = registry.0.lock().unwrap();
    assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
    assert_eq!(
        state.terminal_reason.as_deref(),
        Some("executor_start_failed")
    );
    drop(state);
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert!(driver.ordinary.boundary.try_lock().is_ok());
}

#[tokio::test]
async fn deferred_audit_failure_terminal_commit_failure_reports_command_error() {
    let (driver, _, registry, _, ready) = deferred_audit_fixture().await;
    registry.0.lock().unwrap().fail_finish_once = true;
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        ready.commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        ))),
    )
    .await
    .unwrap()
    .unwrap_err();
    registry.0.lock().unwrap().fail_settlement = false;
    assert!(!error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 0);
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert!(driver.ordinary.boundary.try_lock().is_ok());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let finished = {
                let state = registry.0.lock().unwrap();
                state.task.as_ref().unwrap().status == TaskStatus::Failed && state.finish_calls == 2
            };
            if finished {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        registry.0.lock().unwrap().terminal_reason.as_deref(),
        Some("executor_start_failed")
    );
}

#[tokio::test]
async fn deferred_audit_failure_abort_during_notice_releases_authority() {
    let (driver, _, registry, supervisor, ready) = deferred_audit_fixture().await;
    let release = hold_publication(&driver.script);
    let committing = zeroclaw_spawn::spawn!(async move {
        ready
            .commit(&settings(&goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            )))
            .await
    });
    await_publication(&driver.script).await;
    registry.0.lock().unwrap().fail_settlement = false;
    committing.abort();
    assert!(committing.await.unwrap_err().is_cancelled());
    drop(release);
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert!(driver.ordinary.boundary.try_lock().is_ok());
    tokio::time::timeout(Duration::from_secs(1), async {
        while !supervisor.try_retire().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let state = registry.0.lock().unwrap();
    assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
    assert_eq!(
        state.terminal_reason.as_deref(),
        Some("executor_start_failed")
    );
    assert_eq!(state.transitions, ["start", "finish"]);
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deferred_audit_failure_abort_during_audit_pauses_unlaunched_epoch() {
    let (driver, _, registry, _, ready) = deferred_audit_fixture().await;
    let entered = Arc::new(Semaphore::new(0));
    let wait = Arc::new(Semaphore::new(0));
    {
        let mut state = registry.0.lock().unwrap();
        state.fail_settlement = false;
        state.settlement_entered = Some(entered.clone());
        state.settlement_wait = Some(wait.clone());
    }
    let committing = zeroclaw_spawn::spawn!(async move {
        ready
            .commit(&settings(&goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            )))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let task = registry.0.lock().unwrap().task.clone().unwrap();
    assert_eq!(task.status, TaskStatus::Running);
    committing.abort();
    assert!(committing.await.unwrap_err().is_cancelled());
    assert_unlaunched_epoch_paused(&registry, &task.id, task.execution_epoch).await;
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert!(driver.ordinary.boundary.try_lock().is_ok());
    wait.add_permits(1);
    wait_for_settlement(&registry).await;
}

#[tokio::test]
async fn deferred_audit_failure_worker_ticket_controls_skip_reconciliation() {
    for (command, kind) in [
        (
            GoalCommand::Pause { immediate: false },
            GoalQueuedControl::Pause,
        ),
        (
            GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
            GoalQueuedControl::Budget,
        ),
    ] {
        let (driver, ingress, registry, supervisor) = running_worker().await;
        let scope = {
            let state = registry.0.lock().unwrap();
            let task = state.task.as_ref().unwrap();
            GoalExecutionScope::new(
                task.id.clone(),
                task.session_id.clone().unwrap(),
                task.execution_epoch,
            )
            .unwrap()
        };
        let deferred = queued(
            supervisor
                .admit_control(
                    &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                    ingress,
                    driver.clone(),
                    command,
                )
                .await
                .unwrap(),
            kind,
        );
        let wait = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
        driver.script.release.add_permits(1);
        let ready = tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(driver.script.published.load(Ordering::SeqCst), 1);
        assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
        registry.0.lock().unwrap().fail_settlement = true;
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            ready.commit(&settings(&goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
        )
        .await
        .unwrap()
        .unwrap_err();
        registry.0.lock().unwrap().fail_settlement = false;
        assert!(!error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
        assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
        assert!(driver.ordinary.command.try_lock().is_ok());
        if kind == GoalQueuedControl::Pause {
            assert_eq!(
                registry.0.lock().unwrap().task.as_ref().unwrap().status,
                TaskStatus::Paused
            );
        } else {
            assert_eq!(
                registry
                    .0
                    .lock()
                    .unwrap()
                    .goal
                    .as_ref()
                    .unwrap()
                    .effective_token_limit,
                None
            );
        }
        tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn deferred_audit_failure_competing_start_waits_for_notice() {
    let (driver, ingress, registry, supervisor, ready) = deferred_audit_fixture().await;
    let release = hold_publication(&driver.script);
    let failure = zeroclaw_spawn::spawn!(async move {
        ready
            .commit(&settings(&goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            )))
            .await
    });
    await_publication(&driver.script).await;
    registry.0.lock().unwrap().fail_settlement = false;
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().id.clone();
    let admission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            launch_command(false),
        ),
    )
    .await;
    let deferred = match admission {
        Ok(Ok(GoalControlAdmission::Queued { response, deferred })) => {
            assert_eq!(response, GoalResponse::Queued(GoalQueuedControl::Start));
            deferred
        }
        other => {
            // Consume all owned work before exposing the planned pre-fix RED.
            drop(other);
            failure.abort();
            let _ = failure.await;
            drop(release);
            panic!("a Start must not overtake an unlaunched failure notice");
        }
    };
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().id, predecessor);
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
        assert_eq!(
            state.terminal_reason.as_deref(),
            Some("executor_start_failed")
        );
    }
    let successor = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
    release.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(1), failure)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(
        *driver.script.events.lock().unwrap(),
        [FailureEvent::Notice(GoalExecutionNotice::Failed(
            zeroclaw_goal::GoalFailureReason::ExecutorStartFailed
        ))]
    );
    let ready = tokio::time::timeout(Duration::from_secs(1), successor)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    let committed = tokio::time::timeout(
        Duration::from_secs(1),
        ready.commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        ))),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(committed.response(), GoalResponse::Started(_)));
    assert_ne!(
        registry.0.lock().unwrap().task.as_ref().unwrap().id,
        predecessor
    );
    drop(committed);
}

#[tokio::test]
async fn deferred_audit_failure_competing_start_disposal_withdraws_both() {
    let (driver, ingress, registry, supervisor, ready) = deferred_audit_fixture().await;
    let release = hold_publication(&driver.script);
    let failure = zeroclaw_spawn::spawn!(async move {
        ready
            .commit(&settings(&goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            )))
            .await
    });
    await_publication(&driver.script).await;
    registry.0.lock().unwrap().fail_settlement = false;
    let session = ingress.session_key().durable_id();
    let admission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            launch_command(false),
        ),
    )
    .await;
    let deferred = match admission {
        Ok(Ok(GoalControlAdmission::Queued { response, deferred })) => {
            assert_eq!(response, GoalResponse::Queued(GoalQueuedControl::Start));
            deferred
        }
        other => {
            drop(other);
            failure.abort();
            let _ = failure.await;
            drop(release);
            panic!("disposal fixture requires a Start queued behind the failure notice");
        }
    };
    let successor = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), supervisor.dispose_session(&session))
            .await
            .unwrap()
            .unwrap(),
        zeroclaw_goal::GoalTransitionResult::Applied
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), successor)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    release.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(1), failure)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert!(driver.script.events.lock().unwrap().is_empty());
    assert!(registry.0.lock().unwrap().task.is_none());
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert!(driver.ordinary.boundary.try_lock().is_ok());
}

#[tokio::test]
async fn deferred_audit_failure_competing_start_proceeds_after_teardown() {
    let (driver, ingress, registry, supervisor, ready) = deferred_audit_fixture().await;
    let release = hold_publication(&driver.script);
    let failure = zeroclaw_spawn::spawn!(async move {
        ready
            .commit(&settings(&goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            )))
            .await
    });
    await_publication(&driver.script).await;
    registry.0.lock().unwrap().fail_settlement = false;
    let admission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            launch_command(false),
        ),
    )
    .await;
    let deferred = match admission {
        Ok(Ok(GoalControlAdmission::Queued { response, deferred })) => {
            assert_eq!(response, GoalResponse::Queued(GoalQueuedControl::Start));
            deferred
        }
        other => {
            drop(other);
            failure.abort();
            let _ = failure.await;
            drop(release);
            panic!("teardown fixture requires a Start queued behind the failure notice");
        }
    };
    // Once ready, B legitimately owns both leases. Observe A's release before
    // allowing the already-registered successor to acquire that authority.
    let (begin_wait, waiting) = tokio::sync::oneshot::channel();
    let successor = zeroclaw_spawn::spawn!(async move {
        waiting.await.unwrap();
        deferred.wait().await
    });
    failure.abort();
    assert!(failure.await.unwrap_err().is_cancelled());
    drop(release);
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert!(driver.ordinary.boundary.try_lock().is_ok());
    begin_wait.send(()).unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(1), successor)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    let committed = tokio::time::timeout(
        Duration::from_secs(1),
        ready.commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        ))),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(committed.response(), GoalResponse::Started(_)));
    drop(committed);
}

#[tokio::test]
async fn unlaunched_failure_rebind_published_then_start_is_direct() {
    let (driver, ingress, _, supervisor) = unlaunched_worker_fixture();
    let GoalControlAdmission::Direct(mut submission) = supervisor
        .admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            launch_command(false),
        )
        .await
        .unwrap()
    else {
        panic!("idle Start must apply directly");
    };
    driver.script.fail_launch_bind.store(true, Ordering::SeqCst);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut submission,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    drop(submission);
    let admission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver,
            launch_command(false),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let GoalControlAdmission::Direct(submission) = admission else {
        panic!("a settled failure notice must not defer Start");
    };
    assert!(matches!(submission.response(), GoalResponse::Started(_)));
    drop(submission);
}

async fn running_worker_on(
    coordinator: Option<&zeroclaw_goal::GoalExecutionRestartCoordinator>,
) -> (
    Arc<WorkerDriver>,
    GoalIngressContext,
    Arc<Registry>,
    Arc<GoalExecutionSupervisor>,
) {
    let config = goal_config_with(Some(1000), None, "openai.default", true);
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
    let (ordinary, ingress) = serialized_fixture();
    let driver = Arc::new(WorkerDriver {
        ordinary,
        script: Arc::new(Script::default()),
    });
    let registry = Arc::new(Registry::default());
    let runtime = GoalRuntime::new(registry.clone());
    let engine = Arc::new(runtime.execution_engine(Arc::new(NoPhysicalAttempts)));
    let supervisor = match coordinator {
        Some(coordinator) => coordinator.new_supervisor(engine).await,
        None => Arc::new(GoalExecutionSupervisor::new(engine)),
    };
    let mut submission = supervisor
        .submit(
            &settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "Complete the bounded fixture".into(),
            },
        )
        .await
        .unwrap();
    supervisor
        .launch_after_response(&settings_source, &mut submission)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), driver.script.entered.acquire())
        .await
        .expect("actual parent operation must have started")
        .unwrap()
        .forget();
    (driver, ingress, registry, supervisor)
}

#[tokio::test]
async fn restart_pause_failure_closes_checkpoint_before_draining_all_hosts() {
    let coordinator = Arc::new(zeroclaw_goal::GoalExecutionRestartCoordinator::new());
    let (first, ingress, registry, supervisor) = running_worker_on(Some(&coordinator)).await;
    let (second, _, second_registry, _second_supervisor) =
        running_worker_on(Some(&coordinator)).await;
    first.script.continue_verifier.store(true, Ordering::SeqCst);
    let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let scope = GoalExecutionScope::new(
        task.id,
        ingress.session_key().durable_id(),
        task.execution_epoch,
    )
    .unwrap();
    let failed = Arc::new(Semaphore::new(0));
    let lookup_failed = Arc::new(Semaphore::new(0));
    {
        let mut state = registry.0.lock().unwrap();
        state.fail_restart_pause_once = true;
        state.restart_pause_failed = Some(failed.clone());
        state.fail_lookup_after_restart_pause = true;
        state.lookup_failed = Some(lookup_failed.clone());
    }
    let restarting = coordinator.clone();
    let mut restart = zeroclaw_spawn::spawn!(async move { restarting.quiesce_for_restart().await });
    tokio::time::timeout(Duration::from_secs(1), failed.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    tokio::time::timeout(Duration::from_secs(1), lookup_failed.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    tokio::time::timeout(Duration::from_secs(1), async {
        while second_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status
            != TaskStatus::Paused
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        supervisor
            .register_deferred_control(&scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut restart)
            .await
            .is_err()
    );
    assert_eq!(first.script.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.script.verified.load(Ordering::SeqCst), 0);
    first.script.release.add_permits(1);
    second.script.release.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(1), restart)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.to_string(), "fixture restart pause write failed");
    assert_eq!(first.script.published.load(Ordering::SeqCst), 1);
    assert_eq!(first.script.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(
        second_registry
            .0
            .lock()
            .unwrap()
            .goal
            .as_ref()
            .unwrap()
            .pause_reason,
        Some(zeroclaw_goal::GoalPauseReason::DaemonRestart)
    );
    assert!(
        supervisor
            .submit(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress,
                first,
                GoalCommand::Resume { response: None },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn prelaunch_pause_releases_command_before_blocked_predecessor_drains() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let config = goal_config_with(Some(1000), None, "openai.default", true);
    let pause = supervisor
        .submit(
            &settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    assert!(matches!(
        pause.response(),
        GoalResponse::TransitionPending(_)
    ));
    drop(pause);
    let mut resume = supervisor
        .submit(
            &settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    assert!(matches!(resume.response(), GoalResponse::Resumed(_)));
    let pausing = supervisor.clone();
    let mut paused = zeroclaw_spawn::spawn!(async move {
        pausing
            .pause_before_launch(&mut resume, zeroclaw_goal::GoalPauseReason::OperatorPaused)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.0.lock().unwrap().task.as_ref().unwrap().status != TaskStatus::Paused {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cancel = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.submit(
            &settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        ),
    )
    .await
    .expect("cancel must acquire command custody before predecessor drain")
    .unwrap();
    assert!(matches!(
        cancel.response(),
        GoalResponse::TransitionPending(_) | GoalResponse::Cancelled(_)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut paused)
            .await
            .is_err()
    );
    assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
    drop(cancel);
    driver.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), paused)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn successor_reads_live_revocation_after_blocked_predecessor_drain() {
    use std::future::Future as _;

    for revoke in [true, false] {
        let (driver, ingress, registry, supervisor) = running_worker().await;
        let config = goal_config_with(Some(1000), None, "openai.default", true);
        let live = LiveConfig::new(config.clone());
        let settings_source = source(&live);
        let pause = supervisor
            .submit(
                &settings(&config),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Pause { immediate: true },
            )
            .await
            .unwrap();
        assert!(matches!(
            pause.response(),
            GoalResponse::TransitionPending(_)
        ));
        drop(pause);
        let mut resume = supervisor
            .submit(
                &settings(&config),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Resume { response: None },
            )
            .await
            .unwrap();
        let GoalResponse::Resumed(projection) = resume.response() else {
            panic!("fixture must admit the successor before draining its predecessor");
        };
        let scope = GoalExecutionScope::new(
            &projection.task_id,
            ingress.session_key().durable_id(),
            projection.execution_epoch,
        )
        .unwrap();
        let lookups_before_drain = registry.0.lock().unwrap().session_lookups;
        let mut launch = Box::pin(supervisor.launch_after_response(&settings_source, &mut resume));
        std::future::poll_fn(|cx| match launch.as_mut().poll(cx) {
            std::task::Poll::Pending => std::task::Poll::Ready(()),
            std::task::Poll::Ready(_) => {
                panic!("successor handoff must wait for the admitted predecessor")
            }
        })
        .await;
        assert_eq!(
            registry.0.lock().unwrap().session_lookups,
            lookups_before_drain + 1,
            "the first poll must reach the predecessor drain's registry lookup"
        );
        assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
        if revoke {
            let mut disabled = config.clone();
            disabled.goal.enabled = false;
            live.publish(live.next_revision().unwrap(), disabled)
                .unwrap();
        }
        driver.script.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(1), launch)
            .await
            .unwrap()
            .unwrap();
        if revoke {
            let error = tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
                .await
                .unwrap()
                .unwrap_err();
            assert!(format!("{error:#}").contains("Goal Mode is disabled"));
            assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
            assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
            assert_eq!(
                registry.0.lock().unwrap().task.as_ref().unwrap().status,
                TaskStatus::Failed
            );
        } else {
            tokio::time::timeout(Duration::from_secs(1), driver.script.entered.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
            assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 2);
            driver.script.release.add_permits(1);
            tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(driver.script.verified.load(Ordering::SeqCst), 1);
            assert_eq!(
                registry.0.lock().unwrap().task.as_ref().unwrap().status,
                TaskStatus::Completed
            );
        }
    }
}

#[tokio::test]
async fn settled_parent_error_preserves_operator_pause() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let scope = GoalExecutionScope::new(
        task.id.clone(),
        ingress.session_key().durable_id(),
        task.execution_epoch,
    )
    .unwrap();
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    let mut pause = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    assert!(matches!(
        pause.response(),
        GoalResponse::TransitionPending(_)
    ));
    driver.script.release.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
        .await
        .unwrap()
        .unwrap_err();
    assert!(format!("{error:#}").contains("fixture parent failed after its lifecycle fence"));
    let late = supervisor
        .launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut pause,
        )
        .await
        .unwrap();
    assert!(matches!(late, Some(GoalResponse::Paused(_))));
    {
        let state = registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        let goal = state.goal.as_ref().unwrap();
        assert_eq!(task.id, scope.task_id());
        assert_eq!(task.status, TaskStatus::Paused);
        assert_eq!(task.execution_epoch, scope.execution_epoch() + 1);
        assert_eq!(
            goal.pause_reason,
            Some(zeroclaw_goal::GoalPauseReason::OperatorPaused)
        );
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
        assert!(state.terminal_reason.is_none());
        assert!(state.pending_settlements.is_empty());
        assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
        assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
        assert_eq!(driver.script.published.load(Ordering::SeqCst), 0);
        assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
        assert!(driver.script.events.lock().unwrap().is_empty());
    }
    assert!(!supervisor.owns_scope(&scope).await);
}

#[tokio::test]
async fn typed_accounting_failures_reach_parent_and_verifier_durable_classification() {
    for verifier in [false, true] {
        for (state, pending, fake, expected) in [
            (
                GoalAccountingState::Missing,
                false,
                false,
                "accounting_missing_or_invalid",
            ),
            (
                GoalAccountingState::Invalid,
                false,
                false,
                "accounting_missing_or_invalid",
            ),
            (
                GoalAccountingState::OutcomeUnknown,
                false,
                false,
                "accounting_outcome_unknown",
            ),
            (
                GoalAccountingState::Complete,
                true,
                false,
                "accounting_outcome_unknown",
            ),
            (
                GoalAccountingState::Invalid,
                false,
                true,
                if verifier {
                    "verifier_operation_failed"
                } else {
                    "parent_operation_failed"
                },
            ),
        ] {
            let (driver, ingress, registry, supervisor) = running_worker().await;
            let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
            let scope = GoalExecutionScope::new(
                task.id,
                ingress.session_key().durable_id(),
                task.execution_epoch,
            )
            .unwrap();
            if verifier {
                driver.script.hold_verifier.store(true, Ordering::SeqCst);
                *driver.script.verifier_accounting_error.lock().unwrap() =
                    Some((state, pending, fake));
                driver.script.release.add_permits(1);
                tokio::time::timeout(
                    Duration::from_secs(1),
                    driver.script.verifier_entered.acquire(),
                )
                .await
                .unwrap()
                .unwrap()
                .forget();
            } else {
                *driver.script.parent_accounting_error.lock().unwrap() =
                    Some((state, pending, fake));
            }
            {
                let mut durable = registry.0.lock().unwrap();
                let goal = durable.goal.as_mut().unwrap();
                goal.accounting_state = if fake {
                    GoalAccountingState::Complete
                } else {
                    state
                };
                if pending {
                    goal.pending_call_id = Some("fixture-pending".into());
                    goal.pending_call_epoch = Some(scope.execution_epoch());
                }
            }
            if verifier {
                driver.script.verifier_release.add_permits(1);
            } else {
                driver.script.release.add_permits(1);
            }
            tokio::time::timeout(Duration::from_secs(1), async {
                while registry.0.lock().unwrap().task.as_ref().unwrap().status != TaskStatus::Failed
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            {
                let durable = registry.0.lock().unwrap();
                assert_eq!(durable.terminal_reason.as_deref(), Some(expected));
                assert_eq!(
                    durable.goal.as_ref().unwrap().accounting_state,
                    if fake {
                        GoalAccountingState::Complete
                    } else {
                        state
                    }
                );
                assert_eq!(
                    durable.goal.as_ref().unwrap().pending_call_id.is_some(),
                    pending
                );
            }
            let error = tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
                .await
                .unwrap()
                .unwrap_err();
            assert!(format!("{error:#}").contains("Goal accounting"));
            let durable = registry.0.lock().unwrap();
            assert_eq!(durable.task.as_ref().unwrap().status, TaskStatus::Failed);
            assert_eq!(durable.terminal_reason.as_deref(), Some(expected));
            assert_eq!(
                durable.goal.as_ref().unwrap().accounting_state,
                if pending {
                    GoalAccountingState::OutcomeUnknown
                } else if fake {
                    GoalAccountingState::Complete
                } else {
                    state
                }
            );
            assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                driver.script.verified.load(Ordering::SeqCst),
                usize::from(verifier)
            );
            assert_eq!(
                driver.script.published.load(Ordering::SeqCst),
                usize::from(verifier)
            );
        }
    }
}

#[tokio::test]
async fn fenced_drain_classifies_durable_incomplete_state_and_original_pending() {
    for (state, pending, expected) in [
        (
            GoalAccountingState::Missing,
            false,
            "accounting_missing_or_invalid",
        ),
        (
            GoalAccountingState::Invalid,
            false,
            "accounting_missing_or_invalid",
        ),
        (
            GoalAccountingState::OutcomeUnknown,
            false,
            "accounting_outcome_unknown",
        ),
        (
            GoalAccountingState::Complete,
            true,
            "accounting_outcome_unknown",
        ),
    ] {
        let (driver, ingress, registry, supervisor) = running_worker().await;
        let original = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
        {
            let mut durable = registry.0.lock().unwrap();
            let goal = durable.goal.as_mut().unwrap();
            goal.accounting_state = state;
            if pending {
                goal.pending_call_id = Some("fixture-pending".into());
                goal.pending_call_epoch = Some(original.execution_epoch);
            }
        }
        driver.script.fail_parent.store(true, Ordering::SeqCst);
        let mut pause = supervisor
            .submit(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress,
                driver.clone(),
                GoalCommand::Pause { immediate: true },
            )
            .await
            .unwrap();
        driver.script.release.add_permits(1);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.launch_after_response(
                &source(&LiveConfig::new(goal_config_with(
                    Some(1000),
                    None,
                    "openai.default",
                    true,
                ))),
                &mut pause,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, Some(GoalResponse::Terminal(_))));
        let durable = registry.0.lock().unwrap();
        let task = durable.task.as_ref().unwrap();
        assert_eq!(task.id, original.id);
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.execution_epoch, original.execution_epoch + 2);
        assert_eq!(durable.terminal_reason.as_deref(), Some(expected));
        let goal = durable.goal.as_ref().unwrap();
        assert_eq!(
            goal.accounting_state,
            if pending {
                GoalAccountingState::OutcomeUnknown
            } else {
                state
            }
        );
        assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
        assert_eq!(durable.pending_settlements.len(), usize::from(pending));
        assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
        assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
        assert_eq!(driver.script.published.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn incomplete_accounting_drain_does_not_overwrite_winning_cancel() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let original = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    registry
        .0
        .lock()
        .unwrap()
        .goal
        .as_mut()
        .unwrap()
        .accounting_state = GoalAccountingState::OutcomeUnknown;
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    let mut cancel = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    driver.script.release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut cancel,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let durable = registry.0.lock().unwrap();
    let task = durable.task.as_ref().unwrap();
    assert_eq!(task.id, original.id);
    assert_eq!(task.status, TaskStatus::Cancelled);
    assert_eq!(task.execution_epoch, original.execution_epoch + 1);
    assert!(durable.terminal_reason.is_none());
    assert_eq!(
        durable.goal.as_ref().unwrap().accounting_state,
        GoalAccountingState::OutcomeUnknown
    );
    assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.published.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fenced_unsettled_call_delivers_accounting_notice_before_late_terminal_response() {
    assert_fenced_parent_failure_delivery(true).await;
}

fn seed_unsettled_call(registry: &Registry) {
    let mut state = registry.0.lock().unwrap();
    let epoch = state.task.as_ref().unwrap().execution_epoch;
    let goal = state.goal.as_mut().unwrap();
    // Seed only durable evidence of an outcome-unknown operation. This is
    // not a fabricated physical attempt or a claim that usage was settled.
    goal.pending_call_id = Some("fixture-unsettled-operation".into());
    goal.pending_call_epoch = Some(epoch);
}

async fn assert_fenced_parent_failure_delivery(unsettled: bool) {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    if unsettled {
        seed_unsettled_call(&registry);
    }
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    let mut pause = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    assert!(matches!(
        pause.response(),
        GoalResponse::TransitionPending(_)
    ));
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Paused
    );
    driver.script.release.add_permits(1);
    let late = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut pause,
        ),
    )
    .await
    .expect("classified parent must settle without another model call")
    .unwrap();
    assert!(matches!(late, Some(GoalResponse::Terminal(_))));
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.published.load(Ordering::SeqCst), 0);
    assert_eq!(
        registry.0.lock().unwrap().terminal_reason.as_deref(),
        Some(if unsettled {
            "accounting_outcome_unknown"
        } else {
            "executor_failed"
        })
    );
    assert_eq!(
        driver.script.notices.load(Ordering::SeqCst),
        1,
        "the closed admission checkpoint must not swallow the drainer's failure notice"
    );
    assert_eq!(
        driver.script.events.lock().unwrap().last(),
        Some(&FailureEvent::Notice(GoalExecutionNotice::Failed(
            if unsettled {
                zeroclaw_goal::GoalFailureReason::OperationOutcomeUnknown
            } else {
                zeroclaw_goal::GoalFailureReason::ExecutorFailed
            }
        )))
    );
    let state = registry.0.lock().unwrap();
    if unsettled {
        let goal = state.goal.as_ref().unwrap();
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert_eq!(goal.accounting_state, GoalAccountingState::OutcomeUnknown);
        assert_eq!(
            state.pending_settlements,
            [GoalAccountingState::OutcomeUnknown]
        );
    } else {
        assert!(state.pending_settlements.is_empty());
    }
}

fn hold_publication(script: &Script) -> tokio::sync::oneshot::Sender<()> {
    let (release, gate) = tokio::sync::oneshot::channel();
    *script.publication_gate.lock().unwrap() = Some(gate);
    release
}

async fn await_publication(script: &Script) {
    tokio::time::timeout(Duration::from_secs(1), script.publication_entered.acquire())
        .await
        .expect("publication must reach the pre-FIFO gate")
        .unwrap()
        .forget();
}

fn assert_original_then_executor_notice(script: &Script) {
    let events = script.events.lock().unwrap();
    assert_eq!(
        events.len(),
        2,
        "one original and one notice, without duplication"
    );
    assert!(matches!(&events[0], FailureEvent::Original(error)
        if error.contains("fixture lifecycle settlement unavailable")));
    assert_eq!(
        events[1],
        FailureEvent::Notice(GoalExecutionNotice::Failed(
            zeroclaw_goal::GoalFailureReason::ExecutorFailed
        ))
    );
}

async fn prepare_completed_failed_tail(
    driver: &Arc<WorkerDriver>,
    registry: &Arc<Registry>,
    supervisor: &Arc<GoalExecutionSupervisor>,
    scope: &GoalExecutionScope,
) {
    let signal = Arc::new(Semaphore::new(0));
    let n = {
        let mut state = registry.0.lock().unwrap();
        let n = state.session_lookups;
        state.fail_finish_once = true;
        state.fail_lookup_at = Some(n + 4);
        state.lookup_failed = Some(signal.clone());
        n
    };
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    driver
        .script
        .reject_execution_incarnation
        .store(true, Ordering::SeqCst);
    driver.script.release.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(1), supervisor.drain(scope))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.to_string(), "fixture session lookup unavailable");
    tokio::time::timeout(Duration::from_secs(1), signal.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(supervisor.owns_scope(scope).await);
    {
        let mut state = registry.0.lock().unwrap();
        assert_eq!(state.session_lookups, n + 4);
        assert_eq!(state.finish_calls, 2);
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
        assert_eq!(state.task.as_ref().unwrap().id, scope.task_id());
        assert_eq!(
            state.task.as_ref().unwrap().execution_epoch,
            scope.execution_epoch() + 1
        );
        assert_eq!(state.terminal_reason.as_deref(), Some("executor_failed"));
        let goal = state.goal.as_ref().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
        state.fail_lookup_at = None;
        state.lookup_failed = None;
    }
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert!(driver.script.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_tail_before_fifo_queues_start_until_original_and_notice_settle() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    registry.0.lock().unwrap().fail_finish_once = true;
    let release = hold_publication(&driver.script);
    driver.script.release.add_permits(1);
    await_publication(&driver.script).await;
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    assert_eq!(predecessor.status, TaskStatus::Failed);
    assert_eq!(
        registry.0.lock().unwrap().terminal_reason.as_deref(),
        Some("executor_failed")
    );
    let snapshot = serde_json::to_value(&predecessor).unwrap();
    for command in [
        GoalCommand::Status,
        GoalCommand::Help,
        GoalCommand::Budget,
        GoalCommand::Resume { response: None },
        GoalCommand::Pause { immediate: false },
        GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
        GoalCommand::Cancel,
        GoalCommand::Pause { immediate: true },
    ] {
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.admit_control(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress.clone(),
                driver.clone(),
                command,
            ),
        )
        .await
        .expect("unsettled publication must not defer other controls")
        .unwrap();
        assert!(matches!(result, GoalControlAdmission::Direct(_)));
        drop(result);
        assert_eq!(
            serde_json::to_value(registry.0.lock().unwrap().task.as_ref().unwrap()).unwrap(),
            snapshot
        );
        assert!(registry.0.lock().unwrap().intents.is_empty());
        assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    }
    // The publication has not joined the real foreground FIFO. Admission must
    // therefore consult publication custody, not just an available turn permit.
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "bounded successor".into(),
                },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().id,
        predecessor.id
    );
    let mut wait = Box::pin(deferred.wait());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut wait)
            .await
            .is_err()
    );
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    assert!(driver.script.events.lock().unwrap().is_empty());
    release.send(()).unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .unwrap()
        .unwrap();
    assert_original_then_executor_notice(&driver.script);
    let committed = ready
        .commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        )))
        .await
        .unwrap();
    assert!(matches!(committed.response(), GoalResponse::Started(_)));
    let successor = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    assert_ne!(successor.id, predecessor.id);
    assert_eq!(
        registry.0.lock().unwrap().transitions,
        ["start", "finish", "start"]
    );
    let mut launch = committed.release();
    driver.script.fail_parent.store(false, Ordering::SeqCst);
    supervisor
        .launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut launch,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), driver.script.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 3);
    let mut cancel = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    driver.script.release.add_permits(1);
    supervisor
        .launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut cancel,
        )
        .await
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_original_then_executor_notice(&driver.script);
}

#[tokio::test]
async fn drainer_delivery_before_fifo_queues_start_without_holding_command_authority() {
    assert_drainer_delivery_queues_start(false).await;
}

#[tokio::test]
async fn unsettled_drainer_delivery_before_fifo_queues_start_until_accounting_notice() {
    assert_drainer_delivery_queues_start(true).await;
}

#[tokio::test]
async fn ack_failure_predecessor_delivery_releases_command_authority() {
    assert_ack_failure_releases_command_authority(false).await;
}

#[tokio::test]
async fn unsettled_ack_failure_predecessor_delivery_releases_command_authority() {
    assert_ack_failure_releases_command_authority(true).await;
}

/// A failed acknowledgement must relinquish command authority before waiting
/// for native failure publication, just like the delivered-response path.
async fn assert_ack_failure_releases_command_authority(unsettled: bool) {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let admitted = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let predecessor_scope = GoalExecutionScope::new(
        admitted.id,
        admitted.session_id.unwrap(),
        admitted.execution_epoch,
    )
    .unwrap();
    if unsettled {
        seed_unsettled_call(&registry);
        driver.script.fail_parent.store(true, Ordering::SeqCst);
    } else {
        prepare_completed_failed_tail(&driver, &registry, &supervisor, &predecessor_scope).await;
        driver
            .script
            .reject_execution_incarnation
            .store(false, Ordering::SeqCst);
    }
    let release = hold_publication(&driver.script);
    let mut pause = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    if unsettled {
        assert!(matches!(
            pause.response(),
            GoalResponse::TransitionPending(_)
        ));
        driver.script.release.add_permits(1);
    } else {
        assert!(matches!(pause.response(), GoalResponse::Terminal(_)));
    }
    let draining_supervisor = supervisor.clone();
    let draining = zeroclaw_spawn::spawn!(async move {
        draining_supervisor
            .pause_before_launch(
                &mut pause,
                zeroclaw_goal::GoalPauseReason::ExternalDependency,
            )
            .await
    });
    await_publication(&driver.script).await;
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    assert_eq!(predecessor.status, TaskStatus::Failed);
    assert!(
        driver.ordinary.command.try_lock().is_ok(),
        "acknowledgement-failure delivery must not own command authority"
    );
    let deferred = queued(
        tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.admit_control(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "bounded successor".into(),
                },
            ),
        )
        .await
        .expect("Start must be acknowledged while failure publication waits")
        .unwrap(),
        GoalQueuedControl::Start,
    );
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().id,
        predecessor.id
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), draining)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    if !unsettled {
        assert_original_then_executor_notice(&driver.script);
    }
    assert_eq!(
        driver.script.events.lock().unwrap().last(),
        Some(&FailureEvent::Notice(GoalExecutionNotice::Failed(
            if unsettled {
                zeroclaw_goal::GoalFailureReason::OperationOutcomeUnknown
            } else {
                zeroclaw_goal::GoalFailureReason::ExecutorFailed
            }
        )))
    );
    assert_eq!(
        registry.0.lock().unwrap().pending_settlements.len(),
        usize::from(unsettled)
    );
    assert!(!supervisor.owns_scope(&predecessor_scope).await);
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    let committed = ready
        .commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        )))
        .await
        .unwrap();
    assert!(matches!(committed.response(), GoalResponse::Started(_)));
    assert_ne!(
        registry.0.lock().unwrap().task.as_ref().unwrap().id,
        predecessor.id
    );
    let mut launch = committed.release();
    supervisor
        .pause_before_launch(&mut launch, zeroclaw_goal::GoalPauseReason::OperatorPaused)
        .await
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unseen_terminal_status_consumes_owed_predecessor_notice() {
    assert_unseen_terminal_consumes_owed_notice(GoalCommand::Status).await;
}

#[tokio::test]
async fn ack_failure_direct_boundary_is_released_before_predecessor_delivery() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let scope = GoalExecutionScope::new(
        task.id.clone(),
        task.session_id.clone().unwrap(),
        task.execution_epoch,
    )
    .unwrap();
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    seed_unsettled_call(&registry);
    let lookup_failed = Arc::new(Semaphore::new(0));
    let n = {
        let mut state = registry.0.lock().unwrap();
        let n = state.session_lookups;
        state.fail_finish = true;
        state.fail_lookup_at = Some(n + 4);
        state.lookup_failed = Some(lookup_failed.clone());
        n
    };
    driver.script.release.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.to_string(), "fixture session lookup unavailable");
    tokio::time::timeout(Duration::from_secs(1), lookup_failed.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(supervisor.owns_scope(&scope).await);
    {
        let mut state = registry.0.lock().unwrap();
        assert_eq!(state.session_lookups, n + 4);
        assert_eq!(state.finish_calls, 2);
        assert_eq!(state.task.as_ref().unwrap().id, task.id);
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
        assert_eq!(
            state.task.as_ref().unwrap().execution_epoch,
            scope.execution_epoch()
        );
        assert!(state.terminal_reason.is_none());
        let goal = state.goal.as_ref().unwrap();
        assert_eq!(
            goal.pending_call_id.as_deref(),
            Some("fixture-unsettled-operation")
        );
        assert_eq!(goal.pending_call_epoch, Some(scope.execution_epoch()));
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(state.pending_settlements.is_empty());
        state.fail_lookup_at = None;
        state.lookup_failed = None;
    }
    assert!(driver.ordinary.boundary.try_lock().is_ok());
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.published.load(Ordering::SeqCst), 0);
    assert!(driver.script.events.lock().unwrap().is_empty());
    let GoalControlAdmission::Direct(mut submission) = supervisor
        .admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap()
    else {
        panic!("VT114(b) requires a direct boundary-bearing admission");
    };
    assert!(
        matches!(
            submission.response(),
            GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Pause)
        ),
        "VT114(b) requires a predecessor obligation after plain pause"
    );
    assert!(driver.ordinary.boundary.try_lock().is_err());
    assert!(driver.ordinary.command.try_lock().is_err());
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().id, task.id);
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Paused);
        assert_eq!(
            state.task.as_ref().unwrap().execution_epoch,
            scope.execution_epoch() + 1
        );
    }
    registry.0.lock().unwrap().fail_finish = false;
    tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.pause_before_launch(
            &mut submission,
            zeroclaw_goal::GoalPauseReason::ExternalDependency,
        ),
    )
    .await
    .expect("acknowledgement failure must release the direct boundary before delivery")
    .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(
        driver.script.events.lock().unwrap().last(),
        Some(&FailureEvent::Notice(GoalExecutionNotice::Failed(
            zeroclaw_goal::GoalFailureReason::OperationOutcomeUnknown
        ),))
    );
    {
        let state = registry.0.lock().unwrap();
        let result = state.task.as_ref().unwrap();
        let goal = state.goal.as_ref().unwrap();
        assert_eq!(result.id, task.id);
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.execution_epoch, scope.execution_epoch() + 2);
        assert_eq!(
            state.terminal_reason.as_deref(),
            Some("accounting_outcome_unknown")
        );
        assert_eq!(goal.accounting_state, GoalAccountingState::OutcomeUnknown);
        assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
        assert_eq!(
            state.pending_settlements,
            [GoalAccountingState::OutcomeUnknown]
        );
        assert_eq!(state.finish_calls, 3);
        assert_eq!(state.transitions, ["start", "pause", "finish"]);
    }
    {
        let events = driver.script.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], FailureEvent::Original(error)
            if error.contains("fixture lifecycle settlement unavailable")
                && error.contains("fixture parent failed after its lifecycle fence")));
        assert_eq!(
            events[1],
            FailureEvent::Notice(GoalExecutionNotice::Failed(
                zeroclaw_goal::GoalFailureReason::OperationOutcomeUnknown
            ))
        );
    }
    assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.published.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 2);
    assert!(!supervisor.owns_scope(&scope).await);
    assert!(driver.ordinary.boundary.try_lock().is_ok());
    assert!(driver.ordinary.command.try_lock().is_ok());
}

#[tokio::test]
async fn unseen_terminal_pause_now_consumes_owed_predecessor_notice() {
    assert_unseen_terminal_consumes_owed_notice(GoalCommand::Pause { immediate: true }).await;
}

/// The shared handoff already retries an owed notice without repeating the
/// original error or changing durable state; both adapters must invoke it.
async fn assert_unseen_terminal_consumes_owed_notice(command: GoalCommand) {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let scope =
        GoalExecutionScope::new(task.id, task.session_id.unwrap(), task.execution_epoch).unwrap();
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    driver.script.fail_notice_once.store(true, Ordering::SeqCst);
    registry.0.lock().unwrap().fail_finish_once = true;
    driver.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        while driver.script.notices.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        driver.script.events.lock().unwrap().as_slice(),
        [FailureEvent::Original(_)]
    ));
    assert!(supervisor.owns_scope(&scope).await);
    let before = {
        let state = registry.0.lock().unwrap();
        (
            serde_json::to_value(&state.task).unwrap(),
            state.terminal_reason.clone(),
            state.transitions.clone(),
            state.finish_calls,
        )
    };
    let acquisitions = driver.script.acquisitions.load(Ordering::SeqCst);
    let GoalControlAdmission::Direct(mut submission) = supervisor
        .admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            command,
        )
        .await
        .unwrap()
    else {
        panic!("a terminal command must not queue");
    };
    assert!(matches!(submission.response(), GoalResponse::Terminal(_)));
    tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.pause_before_launch(
            &mut submission,
            zeroclaw_goal::GoalPauseReason::ExternalDependency,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 2);
    assert_original_then_executor_notice(&driver.script);
    assert!(!supervisor.owns_scope(&scope).await);
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(
            (
                serde_json::to_value(&state.task).unwrap(),
                state.terminal_reason.clone(),
                state.transitions.clone(),
                state.finish_calls
            ),
            before,
        );
        assert!(state.pending_settlements.is_empty());
    }
    assert_eq!(
        driver.script.acquisitions.load(Ordering::SeqCst),
        acquisitions + 1
    );
    // The adapter consumes the completed handoff before accepting another
    // command. Preserve that lifetime on the pre-fix control too.
    drop(submission);
    let GoalControlAdmission::Direct(mut status) = supervisor
        .admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap()
    else {
        panic!("a settled terminal status must not queue");
    };
    supervisor
        .pause_before_launch(
            &mut status,
            zeroclaw_goal::GoalPauseReason::ExternalDependency,
        )
        .await
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 2);
}

async fn assert_drainer_delivery_queues_start(unsettled: bool) {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    if unsettled {
        seed_unsettled_call(&registry);
        driver.script.fail_parent.store(true, Ordering::SeqCst);
    } else {
        let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
        let scope = GoalExecutionScope::new(
            task.id,
            ingress.session_key().durable_id(),
            task.execution_epoch,
        )
        .unwrap();
        prepare_completed_failed_tail(&driver, &registry, &supervisor, &scope).await;
        driver
            .script
            .reject_execution_incarnation
            .store(false, Ordering::SeqCst);
    }
    let release = hold_publication(&driver.script);
    let mut pause = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    if unsettled {
        assert!(matches!(
            pause.response(),
            GoalResponse::TransitionPending(_)
        ));
        driver.script.release.add_permits(1);
    } else {
        assert!(matches!(pause.response(), GoalResponse::Terminal(_)));
    }
    let draining_supervisor = Arc::clone(&supervisor);
    let drain = zeroclaw_spawn::spawn!(async move {
        draining_supervisor
            .launch_after_response(
                &source(&LiveConfig::new(goal_config_with(
                    Some(1000),
                    None,
                    "openai.default",
                    true,
                ))),
                &mut pause,
            )
            .await
    });
    await_publication(&driver.script).await;
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    assert_eq!(predecessor.status, TaskStatus::Failed);
    let admission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "bounded successor".into(),
            },
        ),
    )
    .await
    .expect("delivery must not own command authority")
    .unwrap();
    let predecessor_retained =
        registry.0.lock().unwrap().task.as_ref().unwrap().id == predecessor.id;
    // Release a defective direct admission's lease too, so the baseline reaches
    // the missing-notice observation instead of stopping at the earlier race.
    let deferred = match admission {
        GoalControlAdmission::Queued { response, deferred } => Some((response, deferred)),
        GoalControlAdmission::Direct(direct) => {
            drop(direct);
            None
        }
    };
    let publication_released = release.send(()).is_ok();
    let late = tokio::time::timeout(Duration::from_secs(1), drain)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert!(
        publication_released,
        "the pending publication must retain its gate"
    );
    if unsettled {
        assert!(matches!(late, Some(GoalResponse::Terminal(_))));
    } else {
        assert!(late.is_none());
        assert_original_then_executor_notice(&driver.script);
    }
    assert_eq!(
        driver.script.events.lock().unwrap().last(),
        Some(&FailureEvent::Notice(GoalExecutionNotice::Failed(
            if unsettled {
                zeroclaw_goal::GoalFailureReason::OperationOutcomeUnknown
            } else {
                zeroclaw_goal::GoalFailureReason::ExecutorFailed
            }
        )))
    );
    assert_eq!(
        registry.0.lock().unwrap().pending_settlements.len(),
        usize::from(unsettled)
    );
    assert!(
        predecessor_retained,
        "Start must retain the failed predecessor"
    );
    let (response, deferred) =
        deferred.expect("Start must be Queued until failure delivery settles");
    assert_eq!(response, GoalResponse::Queued(GoalQueuedControl::Start));
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    let committed = ready
        .commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        )))
        .await
        .unwrap();
    assert!(matches!(committed.response(), GoalResponse::Started(_)));
    let mut launch = committed.release();
    supervisor
        .pause_before_launch(&mut launch, zeroclaw_goal::GoalPauseReason::OperatorPaused)
        .await
        .unwrap();
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn disposal_withdraws_failed_tail_and_closes_its_queued_start() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    registry.0.lock().unwrap().fail_finish_once = true;
    let release = hold_publication(&driver.script);
    driver.script.release.add_permits(1);
    await_publication(&driver.script).await;
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    assert_eq!(predecessor.status, TaskStatus::Failed);
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "bounded successor".into(),
                },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.dispose_session(&ingress.session_key().durable_id()),
        )
        .await
        .expect("disposal must withdraw the tail without waiting on its held gate")
        .unwrap(),
        zeroclaw_goal::GoalTransitionResult::Applied
    );
    let closed = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .expect("disposal must close the queued Start")
        .unwrap_err();
    assert!(matches!(closed.response(), GoalResponse::NoCurrentGoal));
    drop(release);
    assert!(registry.0.lock().unwrap().task.is_none());
    assert!(driver.script.events.lock().unwrap().is_empty());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    assert!(supervisor.try_retire().await);
}

#[tokio::test]
async fn restart_closes_queued_start_while_failed_tail_publication_is_pending() {
    let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
    let (driver, ingress, registry, supervisor) = running_worker_on(Some(&coordinator)).await;
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    registry.0.lock().unwrap().fail_finish_once = true;
    let release = hold_publication(&driver.script);
    driver.script.release.add_permits(1);
    await_publication(&driver.script).await;
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    assert_eq!(predecessor.status, TaskStatus::Failed);
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "bounded successor".into(),
                },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    tokio::time::timeout(Duration::from_secs(1), coordinator.quiesce_for_restart())
        .await
        .expect("restart must close queued controls and settle the failed tail")
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .expect("restart must close the queued Start")
        .unwrap_err();
    assert!(matches!(closed.response(), GoalResponse::Terminal(_)));
    drop(release);
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().id, predecessor.id);
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
    }
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 2);
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    {
        let events = driver.script.events.lock().unwrap();
        assert!(matches!(
            events.as_slice(),
            [FailureEvent::Original(_), FailureEvent::Notice(_)]
        ));
    }
    assert!(supervisor.try_retire().await);
}

#[tokio::test]
async fn foreground_blocked_failure_delivery_does_not_hold_lifecycle_finalization() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let scope = GoalExecutionScope::new(
        task.id,
        ingress.session_key().durable_id(),
        task.execution_epoch,
    )
    .unwrap();
    let mut holder = Box::pin(driver.ordinary.boundary.clone().lock_owned());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut holder)
            .await
            .is_err()
    );
    prepare_completed_failed_tail(&driver, &registry, &supervisor, &scope).await;
    let holder = tokio::time::timeout(Duration::from_secs(1), holder)
        .await
        .unwrap();
    driver
        .script
        .reject_execution_incarnation
        .store(false, Ordering::SeqCst);
    let mut pause = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    assert!(matches!(pause.response(), GoalResponse::Terminal(_)));
    let draining_supervisor = Arc::clone(&supervisor);
    let mut drain = zeroclaw_spawn::spawn!(async move {
        draining_supervisor
            .launch_after_response(
                &source(&LiveConfig::new(goal_config_with(
                    Some(1000),
                    None,
                    "openai.default",
                    true,
                ))),
                &mut pause,
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut drain)
            .await
            .is_err(),
        "classified failure delivery must wait for the actual foreground holder"
    );
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.dispose_session(&ingress.session_key().durable_id()),
        )
        .await
        .expect("foreground-blocked publication must release lifecycle finalization")
        .unwrap(),
        zeroclaw_goal::GoalTransitionResult::Applied
    );
    drop(holder);
    tokio::time::timeout(Duration::from_secs(1), drain)
        .await
        .expect("withdrawn delivery must terminate after disposal")
        .unwrap()
        .unwrap();
    assert!(registry.0.lock().unwrap().task.is_none());
    assert!(driver.script.events.lock().unwrap().is_empty());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert!(supervisor.try_retire().await);
}

#[tokio::test]
async fn disposal_withdraws_inflight_drainer_delivery_without_waiting_on_its_latch() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let scope = GoalExecutionScope::new(
        task.id,
        ingress.session_key().durable_id(),
        task.execution_epoch,
    )
    .unwrap();
    prepare_completed_failed_tail(&driver, &registry, &supervisor, &scope).await;
    driver
        .script
        .reject_execution_incarnation
        .store(false, Ordering::SeqCst);
    let release = hold_publication(&driver.script);
    let mut pause = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    assert!(matches!(pause.response(), GoalResponse::Terminal(_)));
    let draining_supervisor = Arc::clone(&supervisor);
    let drain = zeroclaw_spawn::spawn!(async move {
        draining_supervisor
            .launch_after_response(
                &source(&LiveConfig::new(goal_config_with(
                    Some(1000),
                    None,
                    "openai.default",
                    true,
                ))),
                &mut pause,
            )
            .await
    });
    await_publication(&driver.script).await;
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Failed
    );
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.dispose_session(&ingress.session_key().durable_id()),
    )
    .await
    .expect("disposal must not wait for the delivery's held latch or lifecycle lock")
    .unwrap();
    assert_eq!(result, zeroclaw_goal::GoalTransitionResult::Applied);
    tokio::time::timeout(Duration::from_secs(1), drain)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(release);
    assert!(registry.0.lock().unwrap().task.is_none());
    assert!(driver.script.events.lock().unwrap().is_empty());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert!(supervisor.try_retire().await);
}

#[tokio::test]
async fn restart_cleanly_drains_all_hosts_and_requires_explicit_resume() {
    let coordinator = Arc::new(zeroclaw_goal::GoalExecutionRestartCoordinator::new());
    let (first, first_ingress, first_registry, first_supervisor) =
        running_worker_on(Some(&coordinator)).await;
    let (second, second_ingress, second_registry, second_supervisor) =
        running_worker_on(Some(&coordinator)).await;
    let first_task = first_registry
        .0
        .lock()
        .unwrap()
        .task
        .as_ref()
        .unwrap()
        .clone();
    let second_task = second_registry
        .0
        .lock()
        .unwrap()
        .task
        .as_ref()
        .unwrap()
        .clone();
    let first_scope = GoalExecutionScope::new(
        first_task.id.clone(),
        first_ingress.session_key().durable_id(),
        first_task.execution_epoch,
    )
    .unwrap();
    let second_scope = GoalExecutionScope::new(
        second_task.id.clone(),
        second_ingress.session_key().durable_id(),
        second_task.execution_epoch,
    )
    .unwrap();
    let restarting = coordinator.clone();
    let mut restart = zeroclaw_spawn::spawn!(async move { restarting.quiesce_for_restart().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while first_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status
            != TaskStatus::Paused
            || second_registry
                .0
                .lock()
                .unwrap()
                .task
                .as_ref()
                .unwrap()
                .status
                != TaskStatus::Paused
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for (registry, task) in [
        (&first_registry, &first_task),
        (&second_registry, &second_task),
    ] {
        let state = registry.0.lock().unwrap();
        let paused = state.task.as_ref().unwrap();
        let goal = state.goal.as_ref().unwrap();
        assert_eq!(paused.id, task.id);
        assert_eq!(paused.execution_epoch, task.execution_epoch + 1);
        assert_eq!(
            goal.pause_reason,
            Some(zeroclaw_goal::GoalPauseReason::DaemonRestart)
        );
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
    }
    assert!(
        first_supervisor
            .register_deferred_control(&first_scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        second_supervisor
            .register_deferred_control(&second_scope)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut restart)
            .await
            .is_err()
    );
    first.script.release.add_permits(1);
    second.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), restart)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for (driver, supervisor, scope) in [
        (&first, &first_supervisor, &first_scope),
        (&second, &second_supervisor, &second_scope),
    ] {
        assert!(!supervisor.owns_scope(scope).await);
        assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
        assert_eq!(driver.script.published.load(Ordering::SeqCst), 1);
        assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
        assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
        assert!(driver.script.events.lock().unwrap().is_empty());
    }
    let config = goal_config_with(Some(1000), None, "openai.default", true);
    assert!(
        first_supervisor
            .submit(
                &settings(&config),
                first_ingress.clone(),
                first.clone(),
                GoalCommand::Resume { response: None }
            )
            .await
            .is_err()
    );
    coordinator.reopen_for_generation().await;
    for (registry, task, driver) in [
        (&first_registry, &first_task, &first),
        (&second_registry, &second_task, &second),
    ] {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Paused);
        assert_eq!(
            state.task.as_ref().unwrap().execution_epoch,
            task.execution_epoch + 1
        );
        assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 1);
    }
    let live = LiveConfig::new(config.clone());
    let mut resume = first_supervisor
        .submit(
            &settings(&config),
            first_ingress.clone(),
            first.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    assert!(matches!(resume.response(), GoalResponse::Resumed(_)));
    first_supervisor
        .launch_after_response(&source(&live), &mut resume)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), first.script.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let resumed = first_registry
        .0
        .lock()
        .unwrap()
        .task
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(resumed.id, first_task.id);
    assert_eq!(resumed.execution_epoch, first_task.execution_epoch + 2);
    let resumed_scope = GoalExecutionScope::new(
        resumed.id,
        first_ingress.session_key().durable_id(),
        resumed.execution_epoch,
    )
    .unwrap();
    first.script.release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(1),
        first_supervisor.drain(&resumed_scope),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        first_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    assert_eq!(first.script.parent_calls.load(Ordering::SeqCst), 2);
    assert_eq!(first.script.published.load(Ordering::SeqCst), 2);
    assert_eq!(first.script.verified.load(Ordering::SeqCst), 1);
    assert_eq!(
        *first.script.events.lock().unwrap(),
        [FailureEvent::Notice(GoalExecutionNotice::Completed)]
    );
    assert_eq!(first.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(
        second_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status,
        TaskStatus::Paused
    );
    assert_eq!(
        second_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .execution_epoch,
        second_task.execution_epoch + 1
    );
    assert_eq!(
        second_registry
            .0
            .lock()
            .unwrap()
            .goal
            .as_ref()
            .unwrap()
            .pause_reason,
        Some(zeroclaw_goal::GoalPauseReason::DaemonRestart)
    );
    assert_eq!(second.script.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.script.published.load(Ordering::SeqCst), 1);
    assert_eq!(second.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(second.script.notices.load(Ordering::SeqCst), 0);
    assert!(second.script.events.lock().unwrap().is_empty());
    assert!(first_supervisor.try_retire().await);
    assert!(second_supervisor.try_retire().await);
}

#[tokio::test]
async fn restart_drains_all_hosts_before_waiting_for_failure_delivery() {
    let coordinator = Arc::new(zeroclaw_goal::GoalExecutionRestartCoordinator::new());
    let (first, first_ingress, first_registry, first_supervisor) =
        running_worker_on(Some(&coordinator)).await;
    let (second, second_ingress, second_registry, second_supervisor) =
        running_worker_on(Some(&coordinator)).await;
    let first_task = first_registry
        .0
        .lock()
        .unwrap()
        .task
        .as_ref()
        .unwrap()
        .clone();
    let first_scope = GoalExecutionScope::new(
        first_task.id,
        first_ingress.session_key().durable_id(),
        first_task.execution_epoch,
    )
    .unwrap();
    first.script.fail_parent.store(true, Ordering::SeqCst);
    first_registry.0.lock().unwrap().fail_finish_once = true;
    let second_task = second_registry
        .0
        .lock()
        .unwrap()
        .task
        .as_ref()
        .unwrap()
        .clone();
    let second_scope = GoalExecutionScope::new(
        second_task.id,
        second_ingress.session_key().durable_id(),
        second_task.execution_epoch,
    )
    .unwrap();
    let mut holder = Box::pin(first.ordinary.boundary.clone().lock_owned());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut holder)
            .await
            .is_err()
    );
    first.script.release.add_permits(1);
    let holder = tokio::time::timeout(Duration::from_secs(1), holder)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while first_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status
            != TaskStatus::Failed
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    {
        let state = first_registry.0.lock().unwrap();
        assert_eq!(state.finish_calls, 2);
        assert_eq!(state.terminal_reason.as_deref(), Some("executor_failed"));
    }
    assert!(first_supervisor.owns_scope(&first_scope).await);
    let restarting = Arc::clone(&coordinator);
    let mut restart = zeroclaw_spawn::spawn!(async move { restarting.quiesce_for_restart().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while second_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status
            != TaskStatus::Paused
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    second.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        while second_supervisor.owns_scope(&second_scope).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("host two must drain while host one's delivery is blocked");
    assert_eq!(
        first_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    assert_eq!(
        second_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status,
        TaskStatus::Paused
    );
    assert_eq!(
        second_registry
            .0
            .lock()
            .unwrap()
            .goal
            .as_ref()
            .unwrap()
            .pause_reason,
        Some(zeroclaw_goal::GoalPauseReason::DaemonRestart)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut restart)
            .await
            .is_err()
    );
    assert_eq!(first.script.notices.load(Ordering::SeqCst), 0);
    drop(holder);
    tokio::time::timeout(Duration::from_secs(1), restart)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.script.notices.load(Ordering::SeqCst), 1);
    assert_original_then_executor_notice(&first.script);
    assert_eq!(second.script.notices.load(Ordering::SeqCst), 0);
    assert!(first_supervisor.try_retire().await);
    assert!(second_supervisor.try_retire().await);
}

#[tokio::test]
async fn failed_tail_notice_is_retried_once_without_repeating_the_parked_original() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    driver.script.fail_notice_once.store(true, Ordering::SeqCst);
    registry.0.lock().unwrap().fail_finish_once = true;
    let release = hold_publication(&driver.script);
    driver.script.release.add_permits(1);
    await_publication(&driver.script).await;
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "bounded retry successor".into(),
                },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    release.send(()).unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    assert_original_then_executor_notice(&driver.script);
    assert_eq!(
        driver.script.notices.load(Ordering::SeqCst),
        2,
        "one failed attempt, one successful retry"
    );
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 3);
    let committed = ready
        .commit(&settings(&goal_config_with(
            Some(1000),
            None,
            "openai.default",
            true,
        )))
        .await
        .unwrap();
    assert!(matches!(committed.response(), GoalResponse::Started(_)));
    let mut launch = committed.release();
    supervisor
        .pause_before_launch(&mut launch, zeroclaw_goal::GoalPauseReason::OperatorPaused)
        .await
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 2);
    assert_original_then_executor_notice(&driver.script);
    let state = registry.0.lock().unwrap();
    assert_eq!(
        state.finish_calls, 2,
        "one rejected finish, one durable fallback"
    );
    assert!(state.pending_settlements.is_empty());
}

#[tokio::test]
async fn published_failed_tail_is_consumed_once_and_allows_direct_successor() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let scope = GoalExecutionScope::new(
        task.id,
        ingress.session_key().durable_id(),
        task.execution_epoch,
    )
    .unwrap();
    driver.script.fail_parent.store(true, Ordering::SeqCst);
    driver.script.release.add_permits(1);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert!(!supervisor.owns_scope(&scope).await);
    let GoalControlAdmission::Direct(mut successor) = supervisor
        .admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "settled successor".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("settled failure must not defer Start");
    };
    assert!(matches!(successor.response(), GoalResponse::Started(_)));
    supervisor
        .pause_before_launch(
            &mut successor,
            zeroclaw_goal::GoalPauseReason::OperatorPaused,
        )
        .await
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
}

/// Exercise launch reporting through the public handoff, without admitting a
/// provider call or replacing the native failure-notice lease path.
async fn admitted_launch() -> (
    Arc<WorkerDriver>,
    Arc<Registry>,
    Arc<GoalExecutionSupervisor>,
    zeroclaw_goal::GoalExecutionSubmission,
) {
    let (ordinary, ingress) = serialized_fixture();
    let driver = Arc::new(WorkerDriver {
        ordinary,
        script: Arc::new(Script::default()),
    });
    let registry = Arc::new(Registry::default());
    let runtime = GoalRuntime::new(registry.clone());
    let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
        runtime.execution_engine(Arc::new(NoPhysicalAttempts)),
    )));
    let submission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            launch_command(false),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    (driver, registry, supervisor, submission)
}

#[tokio::test]
async fn launch_failure_marker_requires_committed_failure_and_one_notice() {
    let (driver, registry, supervisor, mut submission) = admitted_launch().await;
    driver.script.fail_launch_bind.store(true, Ordering::SeqCst);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut submission,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(
        error.to_string(),
        "Goal launch failure was classified and reported"
    );
    assert!(format!("{error:#}").contains("fixture launch rebind unavailable"));
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(registry.0.lock().unwrap().transitions, ["start", "finish"]);
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn launch_failure_marker_records_a_notice_attempt_not_delivery_success() {
    let (driver, registry, supervisor, mut submission) = admitted_launch().await;
    driver.script.fail_launch_bind.store(true, Ordering::SeqCst);
    driver.script.fail_notice.store(true, Ordering::SeqCst);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut submission,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert!(format!("{error:#}").contains("fixture launch rebind unavailable"));
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(registry.0.lock().unwrap().transitions, ["start", "finish"]);
}

#[tokio::test]
async fn launch_failure_for_noncurrent_execution_has_no_marker_or_notice() {
    let (driver, registry, supervisor, mut submission) = admitted_launch().await;
    // A competing durable fence wins before the launch bind fails.
    registry
        .0
        .lock()
        .unwrap()
        .task
        .as_mut()
        .unwrap()
        .execution_epoch += 1;
    driver.script.fail_launch_bind.store(true, Ordering::SeqCst);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut submission,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(!error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert!(format!("{error:#}").contains("fixture launch rebind unavailable"));
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert_eq!(registry.0.lock().unwrap().transitions, ["start"]);
}

#[tokio::test]
async fn launch_failure_reconciliation_error_remains_unmarked() {
    let (driver, registry, supervisor, mut submission) = admitted_launch().await;
    registry.0.lock().unwrap().fail_finish = true;
    driver.script.fail_launch_bind.store(true, Ordering::SeqCst);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut submission,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(!error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert!(format!("{error:#}").contains("fixture lifecycle settlement unavailable"));
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn transition_pending_drain_error_remains_unmarked() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let mut submission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        submission.response(),
        GoalResponse::TransitionPending(_)
    ));
    // Remove the extension only after the fence is admitted, so its real
    // predecessor drain fails instead of constructing a malformed command.
    registry.0.lock().unwrap().goal.take();
    driver.script.release.add_permits(1);
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut submission,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(!error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn launch_failure_outer_reconciliation_also_marks_a_reported_failure() {
    let (_, registry, supervisor, mut submission) = admitted_launch().await;
    {
        let mut state = registry.0.lock().unwrap();
        state.fail_lookup_at = Some(state.session_lookups + 1);
    }
    // The launch's currentness check fails before either inner reconciliation;
    // only the outer recovery commits and attempts the lifecycle notice.
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut submission,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert!(format!("{error:#}").contains("fixture session lookup unavailable"));
    assert_eq!(registry.0.lock().unwrap().transitions, ["start", "finish"]);
}

#[tokio::test]
async fn launch_failure_reconciliation_keeps_primary_and_marks_only_reported_failure() {
    for cleanup_read_fails in [true, false] {
        let (driver, registry, supervisor, mut submission) = admitted_launch().await;
        let lookup_failed = Arc::new(Semaphore::new(0));
        let (lookups, scope) = {
            let mut state = registry.0.lock().unwrap();
            let task = state.task.as_ref().unwrap();
            let scope = GoalExecutionScope::new(
                task.id.clone(),
                task.session_id.clone().unwrap(),
                task.execution_epoch,
            )
            .unwrap();
            let lookups = state.session_lookups;
            // Failure reconciliation now occurs once: its canonical read is
            // followed by two notice reads. A fourth lookup would be the
            // forbidden duplicate fallback, not a required error boundary.
            state.fail_lookup_at = Some(lookups + if cleanup_read_fails { 1 } else { 4 });
            // Keep failure active through the tracked initial attempt so the
            // retained-duty assertions do not race successful background cleanup.
            state.fail_lookup = cleanup_read_fails;
            state.lookup_failed = Some(lookup_failed.clone());
            (lookups, scope)
        };
        driver.script.fail_launch_bind.store(true, Ordering::SeqCst);
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.launch_after_response(
                &source(&LiveConfig::new(goal_config_with(
                    Some(1000),
                    None,
                    "openai.default",
                    true,
                ))),
                &mut submission,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_native_cleanup_fault(&error, "fixture launch rebind unavailable");
        if cleanup_read_fails {
            // Observe both the awaited reconciliation and tracked initial
            // attempt before restoring storage for the explicit retry.
            wait_cleanup_signal(&lookup_failed).await;
            wait_cleanup_signal(&lookup_failed).await;
            assert!(!error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
            assert!(format!("{error:#}").contains("fixture session lookup unavailable"));
            assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
            {
                let state = registry.0.lock().unwrap();
                assert_eq!(state.transitions, ["start"]);
                assert_eq!(state.session_lookups, lookups + 2);
                assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
                assert_eq!(
                    state.task.as_ref().unwrap().execution_epoch,
                    scope.execution_epoch()
                );
            }
            assert!(supervisor.owns_scope(&scope).await);
            assert!(!retire_without_waiting(&supervisor).await);
            {
                let mut state = registry.0.lock().unwrap();
                state.fail_lookup = false;
                state.fail_lookup_at = None;
            }
            let retained =
                tokio::time::timeout(Duration::from_secs(1), supervisor.pause_for_restart())
                    .await
                    .unwrap()
                    .unwrap_err();
            assert_native_cleanup_fault(&retained, "fixture launch rebind unavailable");
            assert!(format!("{retained:#}").contains("fixture session lookup unavailable"));
            assert_eq!(
                registry.0.lock().unwrap().task.as_ref().unwrap().status,
                TaskStatus::Failed
            );
            assert!(!supervisor.owns_scope(&scope).await);
            assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
        } else {
            assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
            assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
            assert_eq!(registry.0.lock().unwrap().session_lookups, lookups + 3);
            assert!(!supervisor.owns_scope(&scope).await);
        }
        assert_eq!(registry.0.lock().unwrap().transitions, ["start", "finish"]);
        assert!(retire_without_waiting(&supervisor).await);
    }
}

#[tokio::test]
async fn custody_completed_worker_remains_owned_until_exact_drain() {
    let (driver, _, registry, supervisor) = running_worker().await;
    let scope = {
        let state = registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        GoalExecutionScope::new(
            task.id.clone(),
            task.session_id.clone().unwrap(),
            task.execution_epoch,
        )
        .unwrap()
    };
    assert!(!retire_without_waiting(&supervisor).await);
    driver.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if registry.0.lock().unwrap().task.as_ref().unwrap().status == TaskStatus::Completed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!retire_without_waiting(&supervisor).await);
    tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
        .await
        .unwrap()
        .unwrap();
    assert!(retire_without_waiting(&supervisor).await);
}

#[tokio::test]
async fn worker_pause_and_budget_wait_for_published_parent_and_release_before_drain() {
    let config = goal_config();
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
    for (command, kind) in [
        (
            GoalCommand::Pause { immediate: false },
            GoalQueuedControl::Pause,
        ),
        (
            GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
            GoalQueuedControl::Budget,
        ),
    ] {
        let (driver, ingress, registry, supervisor) = running_worker().await;
        let deferred = queued(
            supervisor
                .admit_control(&settings(&config), ingress.clone(), driver.clone(), command)
                .await
                .unwrap(),
            kind,
        );
        assert_eq!(registry.0.lock().unwrap().transitions, ["start"]);
        let wait = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
        tokio::task::yield_now().await;
        assert!(!wait.is_finished());
        driver.script.release.add_permits(1);
        let ready = tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(driver.script.published.load(Ordering::SeqCst), 1);
        assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
        let committed =
            tokio::time::timeout(Duration::from_secs(1), ready.commit(&settings(&config)))
                .await
                .expect("commit must not drain the worker waiting on this ticket")
                .unwrap();
        assert!(driver.ordinary.boundary.clone().try_lock_owned().is_err());
        assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
        let mut submission = committed.release();
        tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.launch_after_response(&settings_source, &mut submission),
        )
        .await
        .expect("release must precede predecessor drain")
        .unwrap();
        if kind == GoalQueuedControl::Pause {
            assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
            assert_eq!(
                registry.0.lock().unwrap().task.as_ref().unwrap().status,
                TaskStatus::Paused
            );
        } else {
            tokio::time::timeout(Duration::from_secs(1), async {
                while registry.0.lock().unwrap().task.as_ref().unwrap().status
                    != TaskStatus::Completed
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(driver.script.verified.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn running_worker_rejects_start_and_resume_without_creating_intents() {
    let config = goal_config();
    let (driver, ingress, registry, supervisor) = running_worker().await;
    for command in [
        GoalCommand::Start {
            budget: GoalBudgetSelection::Defaults,
            objective: "Another goal".into(),
        },
        GoalCommand::Resume {
            response: Some("Choice A\nRetain the limits".into()),
        },
    ] {
        let GoalControlAdmission::Direct(submission) = supervisor
            .admit_control(&settings(&config), ingress.clone(), driver.clone(), command)
            .await
            .unwrap()
        else {
            panic!("invalid controls must not queue on the active worker")
        };
        assert!(matches!(submission.response(), GoalResponse::AlreadyActive));
        drop(submission);
    }
    assert!(registry.0.lock().unwrap().intents.is_empty());
    let scope = {
        let state = registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        GoalExecutionScope::new(
            task.id.clone(),
            task.session_id.clone().unwrap(),
            task.execution_epoch,
        )
        .unwrap()
    };
    driver.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn worker_controls_apply_fifo_without_admitting_a_verifier_between_them() {
    let config = goal_config();
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let first = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress.clone(),
                driver.clone(),
                GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
            )
            .await
            .unwrap(),
        GoalQueuedControl::Budget,
    );
    let second = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Pause { immediate: false },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Pause,
    );
    let second_wait = zeroclaw_spawn::spawn!(async move { second.wait().await });
    driver.script.release.add_permits(1);
    let first_ready = tokio::time::timeout(Duration::from_secs(1), first.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !second_wait.is_finished(),
        "registration order, not waiter order, owns FIFO"
    );
    let mut first_submission = first_ready
        .commit(&settings(&config))
        .await
        .unwrap()
        .release();
    supervisor
        .launch_after_response(&settings_source, &mut first_submission)
        .await
        .unwrap();
    let second_ready = tokio::time::timeout(Duration::from_secs(1), second_wait)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    let mut second_submission = second_ready
        .commit(&settings(&config))
        .await
        .unwrap()
        .release();
    tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(&settings_source, &mut second_submission),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        registry.0.lock().unwrap().transitions,
        ["start", "budget", "pause"]
    );
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    assert!(
        registry
            .0
            .lock()
            .unwrap()
            .intents
            .iter()
            .all(|(_, state)| *state == IntentState::Settled(GoalControlIntentSettlement::Applied))
    );
}

#[tokio::test]
async fn immediate_cancel_closes_a_scheduled_control_before_draining_the_worker() {
    let config = goal_config();
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Pause { immediate: false },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Pause,
    );
    driver.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        while driver.script.published.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Keep the deferred owner alive. Cancel must close its issuing checkpoint;
    // waiting for that owner's release while holding the command lock deadlocks.
    let GoalControlAdmission::Direct(mut cancellation) = supervisor
        .admit_control(
            &settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap()
    else {
        panic!("cancel must never queue")
    };
    tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(&settings_source, &mut cancellation),
    )
    .await
    .expect("immediate fence must close the checkpoint before drain")
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), deferred.wait())
            .await
            .unwrap()
            .is_err()
    );
    wait_for_settlement(&registry).await;
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Cancelled
    );
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(registry.0.lock().unwrap().transitions, ["start", "finish"]);
    assert!(matches!(
        &registry.0.lock().unwrap().intents[0].1,
        IntentState::Settled(GoalControlIntentSettlement::Superseded { .. })
    ));
}

#[tokio::test]
async fn dropping_a_worker_control_abandons_the_intent_and_releases_execution() {
    let config = goal_config();
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Pause { immediate: false },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Pause,
    );
    let scope = {
        let state = registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        GoalExecutionScope::new(
            task.id.clone(),
            task.session_id.clone().unwrap(),
            task.execution_epoch,
        )
        .unwrap()
    };
    drop(deferred);
    wait_for_settlement(&registry).await;
    driver.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Completed
    );
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 1);
    assert!(matches!(
        &registry.0.lock().unwrap().intents[0].1,
        IntentState::Settled(GoalControlIntentSettlement::Abandoned { .. })
    ));
}

#[tokio::test]
async fn settled_pause_and_cancel_do_not_publish_failure_output() {
    for (command, status) in [
        (GoalCommand::Pause { immediate: true }, TaskStatus::Paused),
        (GoalCommand::Cancel, TaskStatus::Cancelled),
    ] {
        let (driver, ingress, registry, supervisor) = running_worker().await;
        let mut submission = supervisor
            .submit(
                &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
                ingress,
                driver.clone(),
                command,
            )
            .await
            .unwrap();
        driver.script.release.add_permits(1);
        tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.launch_after_response(
                &source(&LiveConfig::new(goal_config_with(
                    Some(1000),
                    None,
                    "openai.default",
                    true,
                ))),
                &mut submission,
            ),
        )
        .await
        .expect("settled immediate controls must retire their worker")
        .unwrap();
        assert_eq!(
            registry.0.lock().unwrap().task.as_ref().unwrap().status,
            status
        );
        assert!(driver.script.events.lock().unwrap().is_empty());
        assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
        assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
        assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
        assert!(supervisor.try_retire().await);
    }
}

#[tokio::test]
async fn foreign_task_epoch_and_session_drains_preserve_the_exact_running_owner() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    seed_unsettled_call(&registry);
    let original = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let snapshot = serde_json::to_value(&original).unwrap();
    for scope in [
        GoalExecutionScope::new(
            "foreign-task",
            original.session_id.clone().unwrap(),
            original.execution_epoch,
        )
        .unwrap(),
        GoalExecutionScope::new(
            original.id.clone(),
            original.session_id.clone().unwrap(),
            original.execution_epoch + 2,
        )
        .unwrap(),
        GoalExecutionScope::new(
            original.id.clone(),
            "foreign-session",
            original.execution_epoch,
        )
        .unwrap(),
    ] {
        assert!(
            tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
                .await
                .expect("a foreign drain must not wait on the admitted parent")
                .is_err()
        );
        let state = registry.0.lock().unwrap();
        assert_eq!(
            serde_json::to_value(state.task.as_ref().unwrap()).unwrap(),
            snapshot
        );
        assert!(state.pending_settlements.is_empty());
        assert_eq!(
            state.goal.as_ref().unwrap().pending_call_id.as_deref(),
            Some("fixture-unsettled-operation")
        );
        assert_eq!(
            state.goal.as_ref().unwrap().pending_call_epoch,
            Some(original.execution_epoch)
        );
        assert!(driver.script.events.lock().unwrap().is_empty());
        assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    }
    let mut cancel = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    driver.script.release.add_permits(1);
    supervisor
        .launch_after_response(
            &source(&LiveConfig::new(goal_config_with(
                Some(1000),
                None,
                "openai.default",
                true,
            ))),
            &mut cancel,
        )
        .await
        .unwrap();
    assert!(driver.script.events.lock().unwrap().is_empty());
    assert!(supervisor.try_retire().await);
}

#[tokio::test]
async fn stale_driver_incarnation_rejects_failure_delivery_without_output() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let task = registry.0.lock().unwrap().task.as_ref().unwrap().clone();
    let scope = GoalExecutionScope::new(
        task.id,
        ingress.session_key().durable_id(),
        task.execution_epoch,
    )
    .unwrap();
    prepare_completed_failed_tail(&driver, &registry, &supervisor, &scope).await;
    let mut pause = supervisor
        .submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    assert!(matches!(pause.response(), GoalResponse::Terminal(_)));
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.launch_after_response(
                &source(&LiveConfig::new(goal_config_with(
                    Some(1000),
                    None,
                    "openai.default",
                    true
                ))),
                &mut pause
            )
        )
        .await
        .unwrap()
        .unwrap()
        .is_none()
    );
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
        assert_eq!(state.terminal_reason.as_deref(), Some("executor_failed"));
        assert!(state.pending_settlements.is_empty());
    }
    assert!(driver.script.events.lock().unwrap().is_empty());
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    assert!(supervisor.try_retire().await);
}

#[tokio::test]
async fn completed_predecessor_allows_direct_start_without_failure_delivery() {
    let (driver, ingress, registry, supervisor) = running_worker().await;
    driver.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.0.lock().unwrap().task.as_ref().unwrap().status != TaskStatus::Completed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().id.clone();
    let GoalControlAdmission::Direct(mut successor) = supervisor
        .admit_control(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress,
            driver.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "next bounded fixture".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("a completed predecessor with a free boundary must not queue Start");
    };
    assert!(matches!(successor.response(), GoalResponse::Started(_)));
    {
        let state = registry.0.lock().unwrap();
        assert_ne!(state.task.as_ref().unwrap().id, predecessor);
        assert!(state.intents.is_empty());
    }
    supervisor
        .pause_before_launch(
            &mut successor,
            zeroclaw_goal::GoalPauseReason::OperatorPaused,
        )
        .await
        .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.published.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 1);
    assert!(matches!(
        driver.script.events.lock().unwrap().as_slice(),
        [FailureEvent::Notice(GoalExecutionNotice::Completed)]
    ));
}

#[tokio::test]
async fn completed_worker_falls_back_to_the_ordinary_boundary_without_duplicate_intents() {
    let config = goal_config();
    let (driver, ingress, registry, supervisor) = running_worker().await;
    driver.script.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.0.lock().unwrap().task.as_ref().unwrap().status != TaskStatus::Completed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let ordinary = tokio::time::timeout(
        Duration::from_secs(1),
        driver.ordinary.boundary.clone().lock_owned(),
    )
    .await
    .unwrap();
    assert_eq!(driver.script.notices.load(Ordering::SeqCst), 1);
    let predecessor = registry.0.lock().unwrap().task.as_ref().unwrap().id.clone();
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "Complete the next bounded fixture".into(),
                },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    {
        let state = registry.0.lock().unwrap();
        assert_eq!(state.intents.len(), 1);
        assert_eq!(
            state.intents[0].0.predecessor_task_id.as_deref(),
            Some(predecessor.as_str())
        );
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Completed);
    }
    drop(ordinary);
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    let mut submission = ready.commit(&settings(&config)).await.unwrap().release();
    assert!(matches!(submission.response(), GoalResponse::Started(_)));
    // This case proves admission fallback, not a second provider execution.
    supervisor
        .pause_before_launch(
            &mut submission,
            zeroclaw_goal::GoalPauseReason::OperatorPaused,
        )
        .await
        .unwrap();
    assert_eq!(registry.0.lock().unwrap().intents.len(), 1);
    assert_eq!(driver.script.published.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pause_registered_during_verification_precedes_the_verifiers_completion_transition() {
    let config = goal_config();
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
    let (driver, ingress, registry, supervisor) = running_worker().await;
    driver.script.hold_verifier.store(true, Ordering::SeqCst);
    driver.script.release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(1),
        driver.script.verifier_entered.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Pause { immediate: false },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Pause,
    );
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Running
    );
    assert_eq!(registry.0.lock().unwrap().transitions, ["start"]);
    let wait = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
    tokio::task::yield_now().await;
    assert!(!wait.is_finished());
    driver.script.verifier_release.add_permits(1);
    let ready = tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Running
    );
    let committed = tokio::time::timeout(Duration::from_secs(1), ready.commit(&settings(&config)))
        .await
        .unwrap()
        .unwrap();
    let mut submission = committed.release();
    tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.launch_after_response(&settings_source, &mut submission),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(registry.0.lock().unwrap().transitions, ["start", "pause"]);
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Paused
    );
    assert_eq!(driver.script.published.load(Ordering::SeqCst), 1);
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_ready_without_commit_releases_the_ticket_and_abandons_the_intent() {
    let config = goal_config();
    let (driver, ingress, registry, supervisor) = running_worker().await;
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Pause { immediate: false },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Pause,
    );
    let scope = {
        let state = registry.0.lock().unwrap();
        let task = state.task.as_ref().unwrap();
        GoalExecutionScope::new(
            task.id.clone(),
            task.session_id.clone().unwrap(),
            task.execution_epoch,
        )
        .unwrap()
    };
    driver.script.release.add_permits(1);
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    drop(ready);
    wait_for_settlement(&registry).await;
    tokio::time::timeout(Duration::from_secs(1), supervisor.drain(&scope))
        .await
        .unwrap()
        .unwrap();
    assert!(driver.ordinary.command.try_lock().is_ok());
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Completed
    );
    assert!(matches!(
        &registry.0.lock().unwrap().intents[0].1,
        IntentState::Settled(GoalControlIntentSettlement::Abandoned { .. })
    ));
}

#[derive(Clone, Copy, Debug)]
enum ApplyingRoute {
    Direct,
    DirectBoundary,
    Deferred,
}

async fn apply_supervised_route(
    supervisor: Arc<GoalExecutionSupervisor>,
    driver: Arc<WorkerDriver>,
    ingress: GoalIngressContext,
    route: ApplyingRoute,
    resume: bool,
) -> Result<()> {
    let config = goal_config_with(Some(1000), None, "openai.default", true);
    match route {
        ApplyingRoute::Direct => {
            drop(
                supervisor
                    .submit(&settings(&config), ingress, driver, launch_command(resume))
                    .await?,
            );
        }
        ApplyingRoute::DirectBoundary => {
            drop(
                supervisor
                    .admit_control(&settings(&config), ingress, driver, launch_command(resume))
                    .await?,
            );
        }
        ApplyingRoute::Deferred => {
            let ordinary = driver.ordinary.boundary.clone().lock_owned().await;
            let deferred = queued(
                supervisor
                    .admit_control(&settings(&config), ingress, driver, launch_command(resume))
                    .await?,
                if resume {
                    GoalQueuedControl::Resume
                } else {
                    GoalQueuedControl::Start
                },
            );
            drop(ordinary);
            drop(deferred.wait().await?.commit(&settings(&config)).await?);
        }
    }
    Ok(())
}

async fn wait_cleanup_signal(signal: &Semaphore) {
    tokio::time::timeout(Duration::from_secs(1), signal.acquire())
        .await
        .expect("the exact fixture cleanup boundary must be reached")
        .unwrap()
        .forget();
}

fn fixture_scope(registry: &Registry) -> GoalExecutionScope {
    let state = registry.0.lock().unwrap();
    let task = state.task.as_ref().unwrap();
    GoalExecutionScope::new(
        task.id.clone(),
        task.session_id.clone().unwrap(),
        task.execution_epoch,
    )
    .unwrap()
}

fn assert_no_unlaunched_work(driver: &WorkerDriver, registry: &Registry) {
    assert_eq!(driver.script.acquisitions.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.parent_calls.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(driver.script.published.load(Ordering::SeqCst), 0);
    assert!(registry.0.lock().unwrap().pending_settlements.is_empty());
    let state = registry.0.lock().unwrap();
    assert_eq!(state.pending_admissions, 0);
    assert_eq!(state.accounting_settlements, 0);
    if let Some(goal) = &state.goal {
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    }
}

fn assert_native_cleanup_fault(error: &anyhow::Error, expected: &str) {
    assert!(
        error.chain().any(|cause| {
            cause
                .downcast_ref::<support::CleanupFault>()
                .is_some_and(|fault| fault.0 == expected)
        }),
        "missing actual fixture cause in {error:#}"
    );
}

async fn explicit_status(
    supervisor: &GoalExecutionSupervisor,
    ingress: &GoalIngressContext,
    driver: &Arc<WorkerDriver>,
) -> Result<zeroclaw_goal::GoalExecutionSubmission> {
    tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.submit(
            &settings(&goal_config_with(Some(1000), None, "openai.default", true)),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        ),
    )
    .await
    .expect("an explicit cleanup retry must not self-wait on old command custody")
}

#[tokio::test]
async fn post_commit_projection_cancellation_keeps_custody_on_every_supervised_route() {
    use zeroclaw_goal::{GoalTaskRegistry, GoalTransitionResult};
    for route in [
        ApplyingRoute::Direct,
        ApplyingRoute::DirectBoundary,
        ApplyingRoute::Deferred,
    ] {
        for resume in [false, true] {
            for fault in [None, Some(false), Some(true)] {
                let read_fault = fault == Some(true);
                let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
                if resume {
                    registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
                }
                let entered = Arc::new(Semaphore::new(0));
                let projection = Arc::new(Semaphore::new(0));
                let failed = Arc::new(Semaphore::new(0));
                {
                    let mut state = registry.0.lock().unwrap();
                    state.gate_admitted_projection = true;
                    state.lookup_wait = Some(projection);
                    state.lookup_entered = Some(entered.clone());
                    state.lookup_failed = Some(failed.clone());
                    state.pause_failed = Some(failed.clone());
                }
                let applying = {
                    let supervisor = supervisor.clone();
                    let driver = driver.clone();
                    let ingress = ingress.clone();
                    zeroclaw_spawn::spawn!(async move {
                        apply_supervised_route(supervisor, driver, ingress, route, resume).await
                    })
                };
                wait_cleanup_signal(&entered).await;
                let scope = fixture_scope(&registry);
                assert_eq!(scope.execution_epoch(), if resume { 2 } else { 1 });
                assert!(
                    supervisor.owns_scope(&scope).await,
                    "Applied must arm custody before projection: {route:?}"
                );
                assert!(!retire_without_waiting(&supervisor).await);
                let lookups = registry.0.lock().unwrap().session_lookups;
                {
                    let mut state = registry.0.lock().unwrap();
                    state.fail_lookup = read_fault;
                    state.fail_pause = fault == Some(false);
                }
                applying.abort();
                assert!(applying.await.unwrap_err().is_cancelled());
                if matches!(route, ApplyingRoute::Deferred) {
                    assert!(matches!(
                        wait_for_settlement(&registry).await,
                        IntentState::Settled(
                            zeroclaw_goal::task::GoalControlIntentSettlement::Abandoned { .. }
                        )
                    ));
                }
                if fault.is_none() {
                    assert_unlaunched_epoch_paused(
                        &registry,
                        scope.task_id(),
                        scope.execution_epoch(),
                    )
                    .await;
                    drop(
                        explicit_status(&supervisor, &ingress, &driver)
                            .await
                            .unwrap(),
                    );
                    assert_no_unlaunched_work(&driver, &registry);
                    assert!(!supervisor.owns_scope(&scope).await);
                    assert!(driver.ordinary.command.try_lock().is_ok());
                    assert!(driver.ordinary.boundary.try_lock().is_ok());
                    assert!(retire_without_waiting(&supervisor).await);
                    continue;
                }
                wait_cleanup_signal(&failed).await;
                assert!(supervisor.owns_scope(&scope).await);
                assert!(!retire_without_waiting(&supervisor).await);
                {
                    let state = registry.0.lock().unwrap();
                    assert_eq!(state.session_lookups, lookups + 1);
                    assert_eq!(state.pause_calls, usize::from(!read_fault));
                    let task = state.task.as_ref().unwrap();
                    assert_eq!(task.status, TaskStatus::Running);
                    assert_eq!(task.execution_epoch, scope.execution_epoch());
                    assert_eq!(state.transitions, [if resume { "resume" } else { "start" }]);
                }
                assert!(driver.ordinary.command.try_lock().is_ok());
                assert!(driver.ordinary.boundary.try_lock().is_ok());
                assert_no_unlaunched_work(&driver, &registry);
                assert_eq!(
                    registry
                        .update_session_goal_limits(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            Some(2468),
                            Some(2.0),
                        )
                        .await
                        .unwrap(),
                    GoalTransitionResult::Applied
                );
                {
                    let mut state = registry.0.lock().unwrap();
                    state.fail_lookup = false;
                    state.fail_pause = false;
                }
                let error = explicit_status(&supervisor, &ingress, &driver)
                    .await
                    .unwrap_err();
                assert_native_cleanup_fault(
                    &error,
                    if read_fault {
                        "fixture session lookup unavailable"
                    } else {
                        "fixture cleanup pause unavailable"
                    },
                );
                assert_unlaunched_epoch_paused(&registry, scope.task_id(), scope.execution_epoch())
                    .await;
                {
                    let state = registry.0.lock().unwrap();
                    let goal = state.goal.as_ref().unwrap();
                    assert_eq!(goal.effective_token_limit, Some(2468));
                    assert_eq!(goal.effective_cost_limit_usd, Some(2.0));
                }
                assert!(!supervisor.owns_scope(&scope).await);
                drop(
                    explicit_status(&supervisor, &ingress, &driver)
                        .await
                        .unwrap(),
                );
                assert_no_unlaunched_work(&driver, &registry);
                assert!(retire_without_waiting(&supervisor).await);
            }
        }
    }
}

#[tokio::test]
async fn cancelled_projection_error_fallback_keeps_native_error_and_failed_intent() {
    for route in [
        ApplyingRoute::Direct,
        ApplyingRoute::DirectBoundary,
        ApplyingRoute::Deferred,
    ] {
        for resume in [false, true] {
            let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
            if resume {
                registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
            }
            let projected = Arc::new(Semaphore::new(0));
            let projection = Arc::new(Semaphore::new(0));
            let finishing = Arc::new(Semaphore::new(0));
            {
                let mut state = registry.0.lock().unwrap();
                state.gate_admitted_projection = true;
                state.lookup_wait = Some(projection.clone());
                state.lookup_entered = Some(projected.clone());
                state.finish_wait_at = Some(1);
                state.finish_wait = Some(Arc::new(Semaphore::new(0)));
                state.finish_entered = Some(finishing.clone());
            }
            let applying = {
                let supervisor = supervisor.clone();
                let driver = driver.clone();
                let ingress = ingress.clone();
                zeroclaw_spawn::spawn!(async move {
                    apply_supervised_route(supervisor, driver, ingress, route, resume).await
                })
            };
            wait_cleanup_signal(&projected).await;
            let scope = fixture_scope(&registry);
            {
                let mut state = registry.0.lock().unwrap();
                state.fail_lookup_at = Some(state.session_lookups);
            }
            projection.add_permits(1);
            wait_cleanup_signal(&finishing).await;
            assert!(supervisor.owns_scope(&scope).await);
            registry.0.lock().unwrap().fail_finish = true;
            applying.abort();
            assert!(applying.await.unwrap_err().is_cancelled());
            tokio::time::timeout(Duration::from_secs(1), async {
                while registry.0.lock().unwrap().finish_calls < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("known projection error must transfer Failed intent after cancellation");
            assert!(supervisor.owns_scope(&scope).await);
            assert!(!retire_without_waiting(&supervisor).await);
            {
                let state = registry.0.lock().unwrap();
                assert_eq!(state.finish_calls, 2);
                assert_eq!(
                    state.pause_calls, 0,
                    "a known projection failure is not a cancellation-only pause"
                );
                assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
                assert_eq!(
                    state.task.as_ref().unwrap().execution_epoch,
                    scope.execution_epoch()
                );
            }
            registry.0.lock().unwrap().fail_finish = false;
            let error = explicit_status(&supervisor, &ingress, &driver)
                .await
                .unwrap_err();
            assert_native_cleanup_fault(&error, "fixture session lookup unavailable");
            assert!(format!("{error:#}").contains("fixture lifecycle settlement unavailable"));
            {
                let state = registry.0.lock().unwrap();
                assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
                assert_eq!(
                    state.task.as_ref().unwrap().execution_epoch,
                    scope.execution_epoch() + 1
                );
                assert_eq!(
                    state.terminal_reason.as_deref(),
                    Some("executor_start_failed")
                );
                assert_eq!(state.pause_calls, 0);
                assert_eq!(state.finish_calls, 3);
            }
            drop(
                explicit_status(&supervisor, &ingress, &driver)
                    .await
                    .unwrap(),
            );
            assert_no_unlaunched_work(&driver, &registry);
            assert!(driver.ordinary.command.try_lock().is_ok());
            assert!(driver.ordinary.boundary.try_lock().is_ok());
            assert!(retire_without_waiting(&supervisor).await);
        }
    }
}

#[tokio::test]
async fn cleanup_stale_and_missing_require_a_canonical_winner_without_spinning() {
    use zeroclaw_goal::GoalTransitionResult;
    for transition in [GoalTransitionResult::Stale, GoalTransitionResult::Missing] {
        for fail_reload in [false, true] {
            let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
            let (driver, ingress, registry, _) = unlaunched_worker_fixture();
            let runtime = GoalRuntime::new(registry.clone());
            let supervisor = coordinator
                .new_supervisor(Arc::new(
                    runtime.execution_engine(Arc::new(NoPhysicalAttempts)),
                ))
                .await;
            let submission = supervisor
                .submit(
                    &settings(&goal_config()),
                    ingress.clone(),
                    driver.clone(),
                    launch_command(false),
                )
                .await
                .unwrap();
            let scope = fixture_scope(&registry);
            let baseline = registry.0.lock().unwrap().session_lookups;
            {
                let mut state = registry.0.lock().unwrap();
                state.pause_result = Some(transition);
                if fail_reload {
                    state.fail_lookup_at = Some(baseline + 2);
                }
            }
            drop(submission);
            tokio::time::timeout(Duration::from_secs(1), async {
                while registry.0.lock().unwrap().session_lookups < baseline + 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("Stale/Missing must reload the canonical winner");
            assert!(supervisor.owns_scope(&scope).await);
            assert!(!retire_without_waiting(&supervisor).await);
            let counts = {
                let state = registry.0.lock().unwrap();
                assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
                assert_eq!(
                    state.task.as_ref().unwrap().execution_epoch,
                    scope.execution_epoch()
                );
                assert_eq!(state.pause_calls, 1);
                assert_eq!(state.session_lookups, baseline + 2);
                (state.session_lookups, state.pause_calls)
            };
            // Retirement/ownership observations are not retry triggers.
            for _ in 0..4 {
                assert!(!retire_without_waiting(&supervisor).await);
                tokio::task::yield_now().await;
            }
            assert_eq!(
                {
                    let state = registry.0.lock().unwrap();
                    (state.session_lookups, state.pause_calls)
                },
                counts
            );
            let error =
                tokio::time::timeout(Duration::from_secs(1), coordinator.quiesce_for_restart())
                    .await
                    .unwrap()
                    .unwrap_err();
            if fail_reload {
                assert_native_cleanup_fault(&error, "fixture session lookup unavailable");
            } else {
                assert!(
                    format!("{error:#}")
                        .contains("transition made no progress on its exact Running scope")
                );
            }
            {
                let mut state = registry.0.lock().unwrap();
                assert_eq!(state.pause_calls, 2);
                assert_eq!(state.session_lookups, baseline + 4);
                assert_eq!(
                    state.finish_calls, 0,
                    "ordinary no-progress is not saturation"
                );
                state.pause_result = None;
                state.fail_lookup_at = None;
            }
            assert!(
                coordinator.quiesce_for_restart().await.is_err(),
                "settlement must not erase the retained first fault"
            );
            assert_eq!(
                registry.0.lock().unwrap().task.as_ref().unwrap().status,
                TaskStatus::Paused
            );
            assert!(!supervisor.owns_scope(&scope).await);
            assert_no_unlaunched_work(&driver, &registry);
            coordinator.quiesce_for_restart().await.unwrap();
        }
    }
}

#[tokio::test]
async fn persistent_cleanup_fault_is_retained_after_host_drop_and_blocks_reopening() {
    for read_fault in [false, true] {
        let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
        let (driver, ingress, registry, _) = unlaunched_worker_fixture();
        let runtime = GoalRuntime::new(registry.clone());
        let supervisor = coordinator
            .new_supervisor(Arc::new(
                runtime.execution_engine(Arc::new(NoPhysicalAttempts)),
            ))
            .await;
        let submission = supervisor
            .submit(
                &settings(&goal_config()),
                ingress.clone(),
                driver.clone(),
                launch_command(false),
            )
            .await
            .unwrap();
        let scope = fixture_scope(&registry);
        let failed = Arc::new(Semaphore::new(0));
        {
            let mut state = registry.0.lock().unwrap();
            state.fail_lookup = read_fault;
            state.fail_pause = !read_fault;
            state.lookup_failed = Some(failed.clone());
            state.pause_failed = Some(failed.clone());
        }
        drop(submission);
        wait_cleanup_signal(&failed).await;
        assert!(supervisor.owns_scope(&scope).await);
        assert!(!retire_without_waiting(&supervisor).await);
        let weak_host = Arc::downgrade(&supervisor);
        drop(supervisor);
        assert!(
            weak_host.upgrade().is_some(),
            "the existing coordinator must retain the unresolved host"
        );
        let original = if read_fault {
            "fixture session lookup unavailable"
        } else {
            "fixture cleanup pause unavailable"
        };
        for _ in 0..3 {
            let counts = {
                let state = registry.0.lock().unwrap();
                (state.session_lookups, state.pause_calls)
            };
            let error = tokio::time::timeout(
                Duration::from_secs(1),
                coordinator.quiesce_for_restart(),
            )
            .await
            .expect("persistent storage failure must return, not wait for recovery under admission")
            .unwrap_err();
            assert_native_cleanup_fault(&error, original);
            {
                let state = registry.0.lock().unwrap();
                assert_eq!(state.session_lookups, counts.0 + 1);
                assert_eq!(state.pause_calls, counts.1 + usize::from(!read_fault));
                assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
                assert_eq!(
                    state.task.as_ref().unwrap().execution_epoch,
                    scope.execution_epoch()
                );
                assert_eq!(state.finish_calls, 0);
            }
            coordinator.reopen_for_generation().await;
            let retained = weak_host
                .upgrade()
                .expect("unresolved owner must still be discoverable");
            let error = retained
                .submit(
                    &settings(&goal_config()),
                    ingress.clone(),
                    driver.clone(),
                    launch_command(true),
                )
                .await
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("quiescing"),
                "reopen may not advertise a generation with unresolved cleanup: {error:#}"
            );
            assert!(!retire_without_waiting(&retained).await);
        }
        assert_no_unlaunched_work(&driver, &registry);
        {
            let mut state = registry.0.lock().unwrap();
            state.fail_lookup = false;
            state.fail_pause = false;
        }
        let error = coordinator.quiesce_for_restart().await.unwrap_err();
        assert_native_cleanup_fault(&error, original);
        {
            let state = registry.0.lock().unwrap();
            assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Paused);
            assert_eq!(
                state.task.as_ref().unwrap().execution_epoch,
                scope.execution_epoch() + 1
            );
            assert_eq!(
                state.goal.as_ref().unwrap().pause_reason,
                Some(zeroclaw_goal::GoalPauseReason::DaemonRestart)
            );
            assert_eq!(state.transitions, ["start", "pause"]);
        }
        coordinator.quiesce_for_restart().await.unwrap();
        coordinator.reopen_for_generation().await;
        let next_host = coordinator
            .new_supervisor(Arc::new(
                runtime.execution_engine(Arc::new(NoPhysicalAttempts)),
            ))
            .await;
        let mut resume = next_host
            .submit(
                &settings(&goal_config()),
                ingress.clone(),
                driver.clone(),
                launch_command(true),
            )
            .await
            .unwrap();
        assert!(matches!(resume.response(), GoalResponse::Resumed(_)));
        assert_eq!(
            registry
                .0
                .lock()
                .unwrap()
                .task
                .as_ref()
                .unwrap()
                .execution_epoch,
            scope.execution_epoch() + 2
        );
        next_host
            .pause_before_launch(&mut resume, zeroclaw_goal::GoalPauseReason::OperatorPaused)
            .await
            .unwrap();
        drop(resume);
        assert_no_unlaunched_work(&driver, &registry);
    }
}

#[tokio::test]
async fn restart_fences_and_drains_a_real_second_worker_despite_pending_cleanup_fault() {
    let coordinator = Arc::new(zeroclaw_goal::GoalExecutionRestartCoordinator::new());
    let (driver, ingress, registry, _) = unlaunched_worker_fixture();
    let runtime = GoalRuntime::new(registry.clone());
    let supervisor = coordinator
        .new_supervisor(Arc::new(
            runtime.execution_engine(Arc::new(NoPhysicalAttempts)),
        ))
        .await;
    let submission = supervisor
        .submit(
            &settings(&goal_config()),
            ingress.clone(),
            driver.clone(),
            launch_command(false),
        )
        .await
        .unwrap();
    let scope = fixture_scope(&registry);
    let failed = Arc::new(Semaphore::new(0));
    {
        let mut state = registry.0.lock().unwrap();
        state.fail_lookup = true;
        state.lookup_failed = Some(failed.clone());
    }
    drop(submission);
    wait_cleanup_signal(&failed).await;
    let (second, _, second_registry, second_supervisor) =
        running_worker_on(Some(&coordinator)).await;
    let second_scope = fixture_scope(&second_registry);
    let restart = {
        let coordinator = coordinator.clone();
        zeroclaw_spawn::spawn!(async move { coordinator.quiesce_for_restart().await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while second_registry
            .0
            .lock()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .status
            != TaskStatus::Paused
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all hosts must be fenced before waiting on the admitted parent");
    assert!(
        !restart.is_finished(),
        "the second admitted parent still owns its drain"
    );
    assert!(supervisor.owns_scope(&scope).await);
    assert_eq!(second.script.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.script.verified.load(Ordering::SeqCst), 0);
    second.script.release.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(1), restart)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_native_cleanup_fault(&error, "fixture session lookup unavailable");
    assert!(!second_supervisor.owns_scope(&second_scope).await);
    assert_eq!(second.script.published.load(Ordering::SeqCst), 1);
    assert_eq!(second.script.parent_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.script.verified.load(Ordering::SeqCst), 0);
    assert_eq!(
        second_registry
            .0
            .lock()
            .unwrap()
            .goal
            .as_ref()
            .unwrap()
            .pause_reason,
        Some(zeroclaw_goal::GoalPauseReason::DaemonRestart)
    );
    assert_no_unlaunched_work(&driver, &registry);
    registry.0.lock().unwrap().fail_lookup = false;
    assert!(coordinator.quiesce_for_restart().await.is_err());
    coordinator.quiesce_for_restart().await.unwrap();
}

#[tokio::test]
async fn pending_cleanup_cancel_and_disposal_preserve_the_terminal_winner() {
    for dispose in [false, true] {
        let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
        let submission = supervisor
            .submit(
                &settings(&goal_config()),
                ingress.clone(),
                driver.clone(),
                launch_command(false),
            )
            .await
            .unwrap();
        let scope = fixture_scope(&registry);
        let failed = Arc::new(Semaphore::new(0));
        {
            let mut state = registry.0.lock().unwrap();
            state.fail_pause = true;
            state.pause_failed = Some(failed.clone());
        }
        drop(submission);
        wait_cleanup_signal(&failed).await;
        let error = if dispose {
            tokio::time::timeout(
                Duration::from_secs(1),
                supervisor.dispose_session(scope.session_id()),
            )
            .await
            .unwrap()
            .unwrap_err()
        } else {
            supervisor
                .submit(
                    &settings(&goal_config()),
                    ingress.clone(),
                    driver.clone(),
                    GoalCommand::Cancel,
                )
                .await
                .unwrap_err()
        };
        assert_native_cleanup_fault(&error, "fixture cleanup pause unavailable");
        {
            let state = registry.0.lock().unwrap();
            let task = state
                .task
                .as_ref()
                .expect("an error must not claim session deletion");
            assert_eq!(task.id, scope.task_id());
            assert_eq!(task.status, TaskStatus::Cancelled);
            assert_eq!(task.execution_epoch, scope.execution_epoch() + 1);
            assert_eq!(state.transitions, ["start", "finish"]);
            assert_eq!(
                state.pause_calls, 1,
                "an authorized terminal fence must not wait on the rejected pause"
            );
            assert_eq!(state.delete_calls, 0);
        }
        assert!(!supervisor.owns_scope(&scope).await);
        assert_no_unlaunched_work(&driver, &registry);
        registry.0.lock().unwrap().fail_delete = true;
        let error = supervisor
            .dispose_session(scope.session_id())
            .await
            .unwrap_err();
        assert_native_cleanup_fault(&error, "fixture cleanup delete unavailable");
        assert_eq!(
            registry.0.lock().unwrap().task.as_ref().unwrap().status,
            TaskStatus::Cancelled
        );
        registry.0.lock().unwrap().fail_delete = false;
        assert_eq!(
            supervisor
                .dispose_session(scope.session_id())
                .await
                .unwrap(),
            zeroclaw_goal::GoalTransitionResult::Applied
        );
        {
            let state = registry.0.lock().unwrap();
            assert!(state.task.is_none());
            assert!(state.goal.is_none());
            assert_eq!(state.delete_calls, 2);
        }
        assert!(driver.ordinary.command.try_lock().is_ok());
        assert!(driver.ordinary.boundary.try_lock().is_ok());
        assert!(retire_without_waiting(&supervisor).await);
    }
}

#[tokio::test]
async fn cleanup_task_panic_and_cancelled_explicit_retry_keep_the_exact_owner() {
    for panic_initial in [false, true] {
        let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
        let submission = supervisor
            .submit(
                &settings(&goal_config()),
                ingress.clone(),
                driver.clone(),
                launch_command(false),
            )
            .await
            .unwrap();
        let scope = fixture_scope(&registry);
        let failed = Arc::new(Semaphore::new(0));
        let baseline = registry.0.lock().unwrap().session_lookups;
        {
            let mut state = registry.0.lock().unwrap();
            state.panic_lookup_once = panic_initial;
            state.fail_pause = true;
            state.pause_failed = Some(failed.clone());
        }
        drop(submission);
        if panic_initial {
            tokio::time::timeout(Duration::from_secs(1), async {
                while registry.0.lock().unwrap().session_lookups == baseline {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let error = explicit_status(&supervisor, &ingress, &driver)
                .await
                .unwrap_err();
            assert!(
                error.chain().any(|cause| cause
                    .downcast_ref::<tokio::task::JoinError>()
                    .is_some_and(tokio::task::JoinError::is_panic)),
                "the actual cleanup JoinError must survive: {error:#}"
            );
        } else {
            wait_cleanup_signal(&failed).await;
        }
        assert!(supervisor.owns_scope(&scope).await);
        assert!(!retire_without_waiting(&supervisor).await);
        let entered = Arc::new(Semaphore::new(0));
        {
            let mut state = registry.0.lock().unwrap();
            state.fail_pause = false;
            state.lookup_wait_at = Some(state.session_lookups + 1);
            state.lookup_wait = Some(Arc::new(Semaphore::new(0)));
            state.lookup_entered = Some(entered.clone());
        }
        let retry = {
            let supervisor = supervisor.clone();
            let ingress = ingress.clone();
            let driver = driver.clone();
            zeroclaw_spawn::spawn!(
                async move { explicit_status(&supervisor, &ingress, &driver).await }
            )
        };
        wait_cleanup_signal(&entered).await;
        assert!(supervisor.owns_scope(&scope).await);
        retry.abort();
        assert!(retry.await.unwrap_err().is_cancelled());
        assert!(supervisor.owns_scope(&scope).await);
        assert!(!retire_without_waiting(&supervisor).await);
        let error = explicit_status(&supervisor, &ingress, &driver)
            .await
            .unwrap_err();
        if panic_initial {
            assert!(error.chain().any(|cause| {
                cause
                    .downcast_ref::<tokio::task::JoinError>()
                    .is_some_and(tokio::task::JoinError::is_panic)
            }));
        } else {
            assert_native_cleanup_fault(&error, "fixture cleanup pause unavailable");
        }
        assert_unlaunched_epoch_paused(&registry, scope.task_id(), scope.execution_epoch()).await;
        drop(
            explicit_status(&supervisor, &ingress, &driver)
                .await
                .unwrap(),
        );
        assert!(!supervisor.owns_scope(&scope).await);
        assert_no_unlaunched_work(&driver, &registry);
        assert!(driver.ordinary.command.try_lock().is_ok());
        assert!(driver.ordinary.boundary.try_lock().is_ok());
        assert!(retire_without_waiting(&supervisor).await);
    }
}

#[tokio::test]
async fn failed_bind_and_audit_finish_keep_native_primary_and_failed_cleanup_intent() {
    for audit in [false, true] {
        let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
        let config = goal_config_with(Some(1000), None, "openai.default", true);
        let error;
        let scope;
        if audit {
            let ordinary = driver.ordinary.boundary.clone().lock_owned().await;
            let deferred = queued(
                supervisor
                    .admit_control(
                        &settings(&config),
                        ingress.clone(),
                        driver.clone(),
                        launch_command(false),
                    )
                    .await
                    .unwrap(),
                GoalQueuedControl::Start,
            );
            drop(ordinary);
            let ready = deferred.wait().await.unwrap();
            {
                let mut state = registry.0.lock().unwrap();
                state.fail_settlement = true;
                state.fail_finish = true;
            }
            error = ready.commit(&settings(&config)).await.unwrap_err();
            scope = fixture_scope(&registry);
            registry.0.lock().unwrap().fail_settlement = false;
        } else {
            let mut submission = supervisor
                .submit(
                    &settings(&config),
                    ingress.clone(),
                    driver.clone(),
                    launch_command(false),
                )
                .await
                .unwrap();
            scope = fixture_scope(&registry);
            registry.0.lock().unwrap().fail_finish = true;
            driver.script.fail_launch_bind.store(true, Ordering::SeqCst);
            let live = LiveConfig::new(config.clone());
            error = supervisor
                .launch_after_response(&source(&live), &mut submission)
                .await
                .unwrap_err();
            drop(submission);
        }
        let original = if audit {
            "fixture intent settlement unavailable"
        } else {
            "fixture launch rebind unavailable"
        };
        let actual_primary = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<support::CleanupFault>())
            .expect("native original must remain in the source chain");
        assert_eq!(
            actual_primary.0, original,
            "cleanup must not replace the actual original: {error:#}"
        );
        assert!(format!("{error:#}").contains("fixture lifecycle settlement unavailable"));
        assert!(!error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.0.lock().unwrap().finish_calls < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed awaited fallback must transfer to one retained initial attempt");
        assert!(supervisor.owns_scope(&scope).await);
        assert!(!retire_without_waiting(&supervisor).await);
        {
            let state = registry.0.lock().unwrap();
            assert_eq!(
                state.finish_calls, 2,
                "one fallback plus one tracked initial attempt, not nested reconciliation"
            );
            assert_eq!(
                state.pause_calls, 0,
                "a known failure cannot degrade to cancellation pause"
            );
            assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
            assert_eq!(
                state.task.as_ref().unwrap().execution_epoch,
                scope.execution_epoch()
            );
        }
        assert_eq!(driver.script.notices.load(Ordering::SeqCst), 0);
        assert_no_unlaunched_work(&driver, &registry);
        assert!(driver.ordinary.command.try_lock().is_ok());
        assert!(driver.ordinary.boundary.try_lock().is_ok());
        registry.0.lock().unwrap().fail_finish = false;
        let error = explicit_status(&supervisor, &ingress, &driver)
            .await
            .unwrap_err();
        let retained_primary = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<support::CleanupFault>())
            .expect("retry must preserve its already-known native original");
        assert_eq!(retained_primary.0, original);
        assert!(format!("{error:#}").contains("fixture lifecycle settlement unavailable"));
        {
            let state = registry.0.lock().unwrap();
            assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Failed);
            assert_eq!(
                state.task.as_ref().unwrap().execution_epoch,
                scope.execution_epoch() + 1
            );
            assert_eq!(
                state.terminal_reason.as_deref(),
                Some("executor_start_failed")
            );
            assert_eq!(state.finish_calls, 3);
            assert_eq!(state.pause_calls, 0);
            assert_eq!(state.transitions, ["start", "finish"]);
        }
        assert!(!supervisor.owns_scope(&scope).await);
        drop(
            explicit_status(&supervisor, &ingress, &driver)
                .await
                .unwrap(),
        );
        assert!(driver.script.notices.load(Ordering::SeqCst) <= 1);
        assert_no_unlaunched_work(&driver, &registry);
        assert!(retire_without_waiting(&supervisor).await);
    }
}

#[tokio::test]
async fn cleanup_rereads_and_preserves_real_pause_terminal_and_absent_winners() {
    use support::PauseWinner;
    use zeroclaw_goal::{
        GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, GoalTransitionResult,
    };
    for winner in [
        Some(TaskStatus::Paused),
        Some(TaskStatus::Completed),
        Some(TaskStatus::Cancelled),
        None,
    ] {
        let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
        let submission = supervisor
            .submit(
                &settings(&goal_config()),
                ingress,
                driver.clone(),
                launch_command(false),
            )
            .await
            .unwrap();
        let scope = fixture_scope(&registry);
        let pause = GoalPauseState {
            reason: GoalPauseReason::NeedsUserInput,
            description: Some("operator-owned opaque description".into()),
            blockers: vec![GoalBlocker {
                kind: GoalBlockerKind::NeedsUserInput,
                message: "retain this question".into(),
                payload: Some(serde_json::json!({"opaque": [1, {"preserve": true}]})),
            }],
        };
        let expected_goal = {
            let mut state = registry.0.lock().unwrap();
            let goal = state.goal.as_mut().unwrap();
            goal.effective_token_limit = Some(4321);
            goal.effective_cost_limit_usd = Some(12.5);
            if winner == Some(TaskStatus::Paused) {
                goal.pause_reason = Some(pause.reason);
                goal.pause_description = pause.description.clone();
                goal.blockers = pause.blockers.clone();
            }
            let expected = goal.clone();
            state.pause_result = Some(if winner.is_none() {
                GoalTransitionResult::Missing
            } else {
                GoalTransitionResult::Stale
            });
            state.pause_winner = Some(match winner {
                Some(TaskStatus::Paused) => PauseWinner::Pause(pause),
                None => PauseWinner::Delete,
                Some(status) => PauseWinner::Finish(status),
            });
            expected
        };
        let baseline = registry.0.lock().unwrap().session_lookups;
        drop(submission);
        tokio::time::timeout(Duration::from_secs(1), async {
            while supervisor.owns_scope(&scope).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("verified canonical winner must release only the old cleanup owner");
        {
            let state = registry.0.lock().unwrap();
            assert_eq!(
                state.session_lookups,
                baseline + 2,
                "result label must be followed by winner reload"
            );
            assert_eq!(state.pause_calls, 1);
            assert_eq!(
                state.finish_calls, 0,
                "cleanup must not overwrite a competing winner"
            );
            if let Some(winner) = winner {
                let task = state.task.as_ref().unwrap();
                assert_eq!(task.id, scope.task_id());
                assert_eq!(task.status, winner);
                assert_eq!(task.execution_epoch, scope.execution_epoch() + 1);
                assert_eq!(state.goal.as_ref().unwrap(), &expected_goal);
                if winner.is_terminal() {
                    assert_eq!(
                        state.terminal_reason.as_deref(),
                        Some("fixture competing winner")
                    );
                }
            } else {
                assert!(state.task.is_none());
                assert!(state.goal.is_none());
                assert_eq!(state.transitions, ["start", "delete"]);
            }
        }
        assert_no_unlaunched_work(&driver, &registry);
        assert!(retire_without_waiting(&supervisor).await);
    }
}

#[tokio::test]
async fn cleanup_rejects_unowned_newer_running_epoch_or_replacement_without_retargeting() {
    use zeroclaw_goal::{GoalPauseReason, GoalPauseState, GoalTaskRegistry, GoalTransitionResult};
    for replacement in [false, true] {
        let (driver, ingress, registry, supervisor) = unlaunched_worker_fixture();
        let submission = supervisor
            .submit(
                &settings(&goal_config()),
                ingress.clone(),
                driver.clone(),
                launch_command(false),
            )
            .await
            .unwrap();
        let scope = fixture_scope(&registry);
        if replacement {
            assert_eq!(
                registry
                    .finish_session_goal(
                        scope.task_id(),
                        scope.session_id(),
                        scope.execution_epoch(),
                        TaskStatus::Cancelled,
                        Some("old owner replaced".into())
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
            let mut task = registry.0.lock().unwrap().task.clone().unwrap();
            let mut goal = registry.0.lock().unwrap().goal.clone().unwrap();
            task.id = "fixture-replacement".into();
            task.status = TaskStatus::Running;
            task.execution_epoch = 1;
            goal.task_id = task.id.clone();
            goal.effective_token_limit = Some(8765);
            assert_eq!(
                registry
                    .create_or_replace_session_goal(task, goal)
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
        } else {
            assert_eq!(
                registry
                    .pause_session_goal(
                        scope.task_id(),
                        scope.session_id(),
                        scope.execution_epoch(),
                        GoalPauseState {
                            reason: GoalPauseReason::OperatorPaused,
                            description: None,
                            blockers: vec![]
                        }
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
            assert_eq!(
                registry
                    .resume_session_goal(
                        scope.task_id(),
                        scope.session_id(),
                        scope.execution_epoch() + 1,
                        1,
                        "fixture-boot"
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
        }
        let successor = registry.0.lock().unwrap().task.clone().unwrap();
        let successor_goal = registry.0.lock().unwrap().goal.clone().unwrap();
        let baseline = registry.0.lock().unwrap().session_lookups;
        let pauses = registry.0.lock().unwrap().pause_calls;
        let finishes = registry.0.lock().unwrap().finish_calls;
        drop(submission);
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.0.lock().unwrap().session_lookups == baseline {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            supervisor.owns_scope(&scope).await,
            "a newer Running row is not proof of a real successor owner"
        );
        assert!(!retire_without_waiting(&supervisor).await);
        let error = explicit_status(&supervisor, &ingress, &driver)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("Running successor without an exact process-local owner")
        );
        {
            let state = registry.0.lock().unwrap();
            let task = state.task.as_ref().unwrap();
            assert_eq!(task.id, successor.id);
            assert_eq!(task.execution_epoch, successor.execution_epoch);
            assert_eq!(task.status, TaskStatus::Running);
            assert_eq!(state.goal.as_ref().unwrap(), &successor_goal);
            assert_eq!(state.pause_calls, pauses);
            assert_eq!(state.finish_calls, finishes);
        }
        assert_eq!(
            registry
                .finish_session_goal(
                    &successor.id,
                    scope.session_id(),
                    successor.execution_epoch,
                    TaskStatus::Cancelled,
                    Some("authorized successor winner".into())
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        assert!(
            explicit_status(&supervisor, &ingress, &driver)
                .await
                .is_err(),
            "settlement must retain the earlier missing-owner diagnostic"
        );
        drop(
            explicit_status(&supervisor, &ingress, &driver)
                .await
                .unwrap(),
        );
        assert!(!supervisor.owns_scope(&scope).await);
        assert_no_unlaunched_work(&driver, &registry);
        assert!(retire_without_waiting(&supervisor).await);
    }
}

struct RecordingCleanupDiagnosticGuard {
    inputs: std::sync::Mutex<Vec<String>>,
    entered: Semaphore,
}

impl Default for RecordingCleanupDiagnosticGuard {
    fn default() -> Self {
        Self {
            inputs: std::sync::Mutex::new(Vec::new()),
            entered: Semaphore::new(0),
        }
    }
}

impl zeroclaw_goal::GoalDiagnosticTextGuard for RecordingCleanupDiagnosticGuard {
    fn guard_diagnostic_text(&self, text: &str) -> String {
        self.inputs.lock().unwrap().push(text.to_owned());
        self.entered.add_permits(1);
        "[fixture diagnostic withheld]".to_owned()
    }
}

#[tokio::test]
async fn background_cleanup_diagnostic_uses_guard_without_destroying_native_fault() {
    let (driver, ingress, registry, _) = unlaunched_worker_fixture();
    let guard = Arc::new(RecordingCleanupDiagnosticGuard::default());
    let runtime = GoalRuntime::new(registry.clone());
    let engine = runtime
        .execution_engine(Arc::new(NoPhysicalAttempts))
        .with_diagnostic_text_guard(guard.clone());
    let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(engine)));
    let submission = supervisor
        .submit(
            &settings(&goal_config()),
            ingress.clone(),
            driver.clone(),
            launch_command(false),
        )
        .await
        .unwrap();
    let scope = fixture_scope(&registry);
    // Deliberately synthetic, not a credential or an operator-owned path.
    let diagnostic =
        "fixture lookup rejected sk-test-not-a-real-secret and /fixture/private/content";
    {
        let mut state = registry.0.lock().unwrap();
        state.fail_lookup = true;
        state.lookup_failure_text = Some(diagnostic);
    }
    drop(submission);
    wait_cleanup_signal(&guard.entered).await;
    {
        let inputs = guard.inputs.lock().unwrap();
        assert_eq!(
            inputs.len(),
            1,
            "one tracked attempt must not become a diagnostic retry loop"
        );
        assert!(
            inputs[0].contains(diagnostic),
            "protect the complete dynamic text, not a pre-truncated fragment"
        );
    }
    assert!(supervisor.owns_scope(&scope).await);
    assert!(!retire_without_waiting(&supervisor).await);
    registry.0.lock().unwrap().fail_lookup = false;
    let error = explicit_status(&supervisor, &ingress, &driver)
        .await
        .unwrap_err();
    assert_native_cleanup_fault(&error, diagnostic);
    assert_unlaunched_epoch_paused(&registry, scope.task_id(), scope.execution_epoch()).await;
    assert_no_unlaunched_work(&driver, &registry);
    assert!(retire_without_waiting(&supervisor).await);
}
