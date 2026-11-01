//! VT86 exercises the real executor checkpoint with scripted, non-provider work.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{OwnedMutexGuard, Semaphore};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_goal::{
    GoalParentTurn, GoalVerifierResult, GoalVerifierTurn,
    accounting::{
        GoalOperationAccounting, GoalOperationAccountingFactory, GoalOperationRequest,
        GoalOperationSettlement,
    },
};

struct Script {
    entered: Semaphore,
    release: Semaphore,
    published: AtomicUsize,
    verified: AtomicUsize,
    hold_verifier: AtomicBool,
    verifier_entered: Semaphore,
    verifier_release: Semaphore,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            published: AtomicUsize::new(0),
            verified: AtomicUsize::new(0),
            hold_verifier: AtomicBool::new(false),
            verifier_entered: Semaphore::new(0),
            verifier_release: Semaphore::new(0),
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
        let guard = tokio::select! {
            biased;
            () = checkpoint.wait_closed() => bail!("script execution admission closed"),
            guard = self.ordinary.boundary.clone().lock_owned() => guard,
        };
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

    fn canonical_history(&self) -> Result<Vec<ChatMessage>> {
        Ok(vec![])
    }

    async fn run_parent_turn(
        &mut self,
        _scope: &GoalExecutionScope,
        _turn: GoalParentTurn,
    ) -> Result<String> {
        self.script.entered.add_permits(1);
        self.script.release.acquire().await?.forget();
        Ok("The bounded fixture is complete.".into())
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> Result<()> {
        self.script.published.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn run_verifier(
        &mut self,
        _scope: &GoalExecutionScope,
        _turn: GoalVerifierTurn,
    ) -> Result<GoalVerifierResult> {
        self.script.verified.fetch_add(1, Ordering::SeqCst);
        if self.script.hold_verifier.load(Ordering::SeqCst) {
            self.script.verifier_entered.add_permits(1);
            self.script.verifier_release.acquire().await?.forget();
        }
        Ok(GoalVerifierResult {
            response: r#"{"decision":"complete","reason":"The declared criterion is satisfied.","blockers":[]}"#.into(),
            attribution: None,
        })
    }
}

async fn running_worker() -> (
    Arc<WorkerDriver>,
    GoalIngressContext,
    Arc<Registry>,
    Arc<GoalExecutionSupervisor>,
) {
    let config = goal_config();
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
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
