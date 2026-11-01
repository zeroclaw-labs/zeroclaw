use std::sync::{
    Arc, Mutex, Once,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use std::{collections::VecDeque, path::PathBuf, sync::Mutex as StdMutex};
use tempfile::TempDir;
use tokio::{
    sync::Notify,
    time::{Duration, timeout},
};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_commands::goal::GoalCommand;
use zeroclaw_config::{
    api_error::{ConfigApiCode, ConfigApiError},
    cost::CostTracker,
    goal::{GoalBudgetLimits as ConfigGoalBudgetLimits, GoalConfig},
    live::LiveConfig,
    providers::ModelProviderRef,
    schema::{Config, OpenAIModelProviderConfig},
};
use zeroclaw_goal::accounting::{
    GoalAccountingFailure, GoalOperationAccounting, GoalOperationAccountingFactory,
    GoalOperationRequest, GoalOperationSettlement,
};
use zeroclaw_goal::{
    GoalController, GoalExecutionHost, GoalExecutionOutcome, GoalExecutionRestartCoordinator,
    GoalExecutionScope, GoalExecutionSupervisor, GoalHostSettings, GoalHostSettingsSource,
    GoalIngressContext, GoalIngressPrincipal, GoalOperationScope, GoalParentTurn,
    GoalParentTurnCheckpoint, GoalParentTurnKind, GoalParentTurnResult, GoalPauseReason,
    GoalResponse, GoalRuntime, GoalSessionBinding, GoalSessionDriver, GoalSessionExecutionLease,
    GoalSessionKey, GoalSessionLease, GoalVerifierResult, GoalVerifierTurn,
};
use zeroclaw_infra::{session_backend::SessionBackend, session_sqlite::SqliteSessionBackend};
use zeroclaw_runtime::control_plane::{
    GoalAccountingState, GoalPauseState, GoalTaskRecord, GoalTaskRegistry, GoalTransitionResult,
    SqliteTaskStore, TaskContinuationContext, TaskRecord, TaskStatus,
};
use zeroclaw_runtime::goal_accounting::{
    ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory,
};

#[path = "support/goal_transition_fault.rs"]
mod goal_transition_fault;
use goal_transition_fault::{
    FinishFaultPauseGate, FinishFaultRegistry, FinishFaultReleaseGate, OriginalTransitionFailure,
    WorkerTransitionFailure,
};

struct RecordingDriver {
    binding: GoalSessionBinding,
    binds: AtomicUsize,
    execution_acquires: AtomicUsize,
}

#[derive(Clone, Copy)]
enum OriginalFailurePoint {
    History,
    Acquisition,
    ResumeAppend,
    HistoryDeliveryFailure,
    HostParent,
    HostVerifier,
    // This is private persistence, never an ordinary host publication.
    HostCandidate,
    CandidateTransition,
    EmptyCandidate,
    HostParentTransition,
    HostVerifierTransition,
    HistoryWorkerTransition,
    CandidateWorkerTransition,
    IncompleteBeforeExecution,
    ReacquisitionFailure,
    ProtocolVerdict,
    AccountingVerdict,
    PricingVerdict,
    CancelledParent,
    ParentFencedPending,
}

impl OriginalFailurePoint {
    fn transition_fault(self) -> Option<&'static str> {
        match self {
            Self::HostParentTransition => Some("parent_operation_failed"),
            Self::HostVerifierTransition => Some("verifier_operation_failed"),
            Self::HistoryWorkerTransition => Some("executor_failed"),
            Self::CandidateTransition => Some("candidate_persistence_failed"),
            _ => None,
        }
    }

    fn reason(self) -> (&'static str, zeroclaw_goal::GoalFailureReason) {
        use zeroclaw_goal::GoalFailureReason;
        match self {
            Self::HostParent => (
                "parent_operation_failed",
                GoalFailureReason::AgentOperationFailed,
            ),
            Self::HostVerifier => (
                "verifier_operation_failed",
                GoalFailureReason::VerifierOperationFailed,
            ),
            Self::HostCandidate => (
                "candidate_persistence_failed",
                GoalFailureReason::CandidatePersistenceFailed,
            ),
            Self::EmptyCandidate => ("candidate_empty", GoalFailureReason::EmptyAgentResponse),
            Self::ProtocolVerdict => (
                "verifier_protocol_invalid",
                GoalFailureReason::VerifierResponseInvalid,
            ),
            Self::AccountingVerdict => (
                "accounting_missing_or_invalid",
                GoalFailureReason::AccountingIncomplete,
            ),
            Self::PricingVerdict => ("pricing_unavailable", GoalFailureReason::PricingIncomplete),
            Self::ParentFencedPending => (
                "accounting_outcome_unknown",
                GoalFailureReason::OperationOutcomeUnknown,
            ),
            Self::CancelledParent => ("operator_cancel", GoalFailureReason::Other),
            _ => ("executor_failed", GoalFailureReason::ExecutorFailed),
        }
    }
}

#[derive(Debug)]
struct OriginalExecutionFailure;

impl std::fmt::Display for OriginalExecutionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("distinctive original execution failure")
    }
}

impl std::error::Error for OriginalExecutionFailure {}

const MISSING_SETTLEMENT_ERROR: &str = "Goal accounting is missing after a durable usage settlement; no further Goal model call will be admitted: accounting state Missing for configured request provider `configured`, model `model` (not an actual-route attribution)";

fn original_native_chain(point: OriginalFailurePoint) -> &'static str {
    match point {
        OriginalFailurePoint::HostParentTransition => {
            "distinctive original Goal transition failure: Goal parent operation failed: distinctive original execution failure"
        }
        OriginalFailurePoint::HostVerifierTransition => {
            "distinctive original Goal transition failure: Goal verifier operation failed: distinctive original execution failure"
        }
        OriginalFailurePoint::CandidateTransition
        | OriginalFailurePoint::CandidateWorkerTransition => {
            "distinctive original Goal transition failure: distinctive original execution failure"
        }
        OriginalFailurePoint::ParentFencedPending => {
            "Goal failure lost its execution fence: Goal parent operation failed: distinctive original execution failure"
        }
        _ => "distinctive original execution failure",
    }
}

#[derive(Default)]
struct OriginalOperationCounts {
    history: AtomicUsize,
    parent: AtomicUsize,
    append: AtomicUsize,
    committed: AtomicUsize,
    verifier: AtomicUsize,
}

struct OriginalErrorDriver {
    binding: GoalSessionBinding,
    point: OriginalFailurePoint,
    acquires: AtomicUsize,
    events: Arc<Mutex<Vec<&'static str>>>,
    published: Arc<Notify>,
    store: Arc<SqliteTaskStore>,
    parent_gate: Option<Arc<OriginalParentGate>>,
    counts: Arc<OriginalOperationCounts>,
}

#[derive(Default)]
struct OriginalParentGate {
    observed: Notify,
    released: Notify,
}

struct OriginalErrorLease {
    session_key: GoalSessionKey,
    point: OriginalFailurePoint,
    events: Arc<Mutex<Vec<&'static str>>>,
    published: Arc<Notify>,
    store: Arc<SqliteTaskStore>,
    parent_gate: Option<Arc<OriginalParentGate>>,
    counts: Arc<OriginalOperationCounts>,
}

#[async_trait]
impl GoalSessionExecutionLease for OriginalErrorLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        self.counts.history.fetch_add(1, Ordering::SeqCst);
        assert!(!matches!(
            self.point,
            OriginalFailurePoint::ResumeAppend
                | OriginalFailurePoint::IncompleteBeforeExecution
                | OriginalFailurePoint::ReacquisitionFailure
        ));
        match self.point {
            OriginalFailurePoint::HostParent
            | OriginalFailurePoint::HostVerifier
            | OriginalFailurePoint::HostCandidate
            | OriginalFailurePoint::CandidateTransition
            | OriginalFailurePoint::EmptyCandidate
            | OriginalFailurePoint::HostParentTransition
            | OriginalFailurePoint::HostVerifierTransition
            | OriginalFailurePoint::CandidateWorkerTransition
            | OriginalFailurePoint::ProtocolVerdict
            | OriginalFailurePoint::AccountingVerdict
            | OriginalFailurePoint::PricingVerdict
            | OriginalFailurePoint::CancelledParent
            | OriginalFailurePoint::ParentFencedPending => Ok(Vec::new()),
            _ => Err(OriginalExecutionFailure.into()),
        }
    }

    async fn append_resume_response(&mut self, response: String) -> anyhow::Result<()> {
        assert!(matches!(self.point, OriginalFailurePoint::ResumeAppend));
        assert_eq!(response, "First line\nSecond line");
        self.events.lock().unwrap().push("resume_append");
        Err(OriginalExecutionFailure.into())
    }

    async fn run_parent_turn(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        self.counts.parent.fetch_add(1, Ordering::SeqCst);
        match self.point {
            OriginalFailurePoint::HostParent | OriginalFailurePoint::HostParentTransition => {
                self.events.lock().unwrap().push("host_error");
                Err(OriginalExecutionFailure.into())
            }
            OriginalFailurePoint::HostVerifier
            | OriginalFailurePoint::HostVerifierTransition
            | OriginalFailurePoint::HostCandidate
            | OriginalFailurePoint::CandidateTransition
            | OriginalFailurePoint::CandidateWorkerTransition => {
                Ok(GoalParentTurnResult::from_continuation_prefix(
                    "parent candidate".into(),
                    turn.working_history,
                ))
            }
            OriginalFailurePoint::EmptyCandidate => Ok(
                GoalParentTurnResult::from_continuation_prefix(String::new(), turn.working_history),
            ),
            OriginalFailurePoint::ProtocolVerdict => {
                Ok(GoalParentTurnResult::from_continuation_prefix(
                    "parent candidate".into(),
                    turn.working_history,
                ))
            }
            OriginalFailurePoint::AccountingVerdict | OriginalFailurePoint::PricingVerdict => {
                use zeroclaw_goal::accounting::{
                    GOAL_OPERATION_ACCOUNTING, GoalOperationRequest, GoalOperationSettlement,
                    GoalUsageEvent,
                };
                let accountant = GOAL_OPERATION_ACCOUNTING.with(Clone::clone).unwrap();
                accountant
                    .admit(GoalOperationRequest::new("configured", "model"))
                    .await?;
                let missing = matches!(self.point, OriginalFailurePoint::AccountingVerdict);
                let settlement = accountant
                    .settle(GoalOperationSettlement {
                        issues: Vec::new(),
                        accounting_state: if missing {
                            GoalAccountingState::Missing
                        } else {
                            GoalAccountingState::Complete
                        },
                        events: if missing {
                            Vec::new()
                        } else {
                            vec![GoalUsageEvent {
                                provider_ref: "unpriced-fallback".into(),
                                model: "unpriced-model".into(),
                                usage: zeroclaw_api::model_provider::TokenUsage {
                                    input_tokens: Some(5),
                                    output_tokens: Some(2),
                                    cached_input_tokens: Some(0),
                                    cache_creation_input_tokens: Some(0),
                                },
                                priceable: true,
                            }]
                        },
                    })
                    .await;
                if let Err(error) = settlement {
                    assert!(missing);
                    let failure = error.downcast_ref::<GoalAccountingFailure>().unwrap();
                    assert_eq!(failure.accounting_state(), GoalAccountingState::Missing);
                    assert!(!failure.has_pending_operation());
                    assert!(error.downcast_ref::<OriginalExecutionFailure>().is_none());
                    assert_eq!(format!("{error:#}"), MISSING_SETTLEMENT_ERROR);
                    let goal = self
                        .store
                        .get_goal_task(operation.execution().task_id())
                        .await?
                        .unwrap();
                    assert_eq!(goal.accounting_state, GoalAccountingState::Missing);
                    assert!(goal.pending_call_id.is_none());
                    assert!(goal.pending_call_epoch.is_none());
                    self.events.lock().unwrap().push("host_error");
                    return Err(error);
                }
                assert!(!missing, "Missing settlement must return its typed failure");
                Ok(GoalParentTurnResult::from_continuation_prefix(
                    "parent candidate".into(),
                    turn.working_history,
                ))
            }
            OriginalFailurePoint::CancelledParent | OriginalFailurePoint::ParentFencedPending => {
                if matches!(self.point, OriginalFailurePoint::ParentFencedPending) {
                    let scope = operation.execution();
                    assert_eq!(
                        self.store
                            .admit_pending_operation(
                                scope.task_id(),
                                scope.session_id(),
                                scope.execution_epoch(),
                                "unsettled-parent-operation",
                            )
                            .await?,
                        GoalTransitionResult::Applied,
                    );
                }
                let gate = self.parent_gate.as_ref().unwrap();
                gate.observed.notify_one();
                gate.released.notified().await;
                self.events.lock().unwrap().push("host_error");
                Err(OriginalExecutionFailure.into())
            }
            _ => panic!("a pre-parent failure must not admit a parent operation"),
        }
    }

    async fn run_verifier(
        &mut self,
        _operation: &GoalOperationScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        self.counts.verifier.fetch_add(1, Ordering::SeqCst);
        if matches!(self.point, OriginalFailurePoint::ProtocolVerdict) {
            return Ok(GoalVerifierResult {
                response: "not a verifier response".into(),
                attribution: None,
            });
        }
        assert!(matches!(
            self.point,
            OriginalFailurePoint::HostVerifier | OriginalFailurePoint::HostVerifierTransition
        ));
        self.events.lock().unwrap().push("host_error");
        Err(OriginalExecutionFailure.into())
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        self.counts.append.fetch_add(1, Ordering::SeqCst);
        match self.point {
            OriginalFailurePoint::HostCandidate
            | OriginalFailurePoint::CandidateTransition
            | OriginalFailurePoint::CandidateWorkerTransition => {
                Err(OriginalExecutionFailure.into())
            }
            OriginalFailurePoint::HostVerifier
            | OriginalFailurePoint::HostVerifierTransition
            | OriginalFailurePoint::ProtocolVerdict
            | OriginalFailurePoint::AccountingVerdict
            | OriginalFailurePoint::PricingVerdict => {
                self.counts.committed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            _ => panic!("a pre-parent failure must not persist a candidate"),
        }
    }

    async fn publish_goal_core_error(&mut self, error: &anyhow::Error) -> anyhow::Result<()> {
        assert!(error.downcast_ref::<OriginalTransitionFailure>().is_none());
        assert!(error.downcast_ref::<WorkerTransitionFailure>().is_none());
        if matches!(self.point, OriginalFailurePoint::IncompleteBeforeExecution) {
            assert_eq!(
                error.to_string(),
                "Goal execution begins with incomplete accounting"
            );
        } else {
            assert!(error.downcast_ref::<OriginalExecutionFailure>().is_some());
            let chain = format!("{error:#}");
            assert_eq!(chain, original_native_chain(self.point));
            assert_eq!(
                chain
                    .matches("distinctive original execution failure")
                    .count(),
                1
            );
            assert!(!chain.contains("distinctive worker Goal transition failure"));
        }
        self.events.lock().unwrap().push("original_error");
        if matches!(self.point, OriginalFailurePoint::HistoryDeliveryFailure) {
            anyhow::bail!("injected original-error delivery failure");
        }
        Ok(())
    }

    async fn publish_goal_notice(
        &mut self,
        notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        assert_eq!(
            notice,
            zeroclaw_goal::GoalExecutionNotice::Failed(self.point.reason().1)
        );
        self.events.lock().unwrap().push("failure_notice");
        self.published.notify_one();
        Ok(())
    }
}

#[async_trait]
impl GoalSessionDriver for OriginalErrorDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        let attempt = self.acquires.fetch_add(1, Ordering::SeqCst);
        if matches!(self.point, OriginalFailurePoint::ReacquisitionFailure)
            || matches!(self.point, OriginalFailurePoint::Acquisition) && attempt == 0
        {
            return Err(OriginalExecutionFailure.into());
        }
        Ok(Box::new(OriginalErrorLease {
            session_key: self.binding.session_key().clone(),
            point: self.point,
            events: Arc::clone(&self.events),
            published: Arc::clone(&self.published),
            store: Arc::clone(&self.store),
            parent_gate: self.parent_gate.clone(),
            counts: Arc::clone(&self.counts),
        }))
    }
}

async fn assert_original_execution_error_precedes_failure_notice(point: OriginalFailurePoint) {
    assert_original_execution_error_for_ingress(point, matrix_ingress()).await;
}

async fn assert_original_execution_error_for_ingress(
    point: OriginalFailurePoint,
    ingress: GoalIngressContext,
) {
    static LOG_INIT: Once = Once::new();
    LOG_INIT.call_once(|| {
        zeroclaw_log::try_install_capture_subscriber();
        drop(zeroclaw_log::subscribe_or_install());
    });
    let mut log_events = zeroclaw_log::subscribe().unwrap();
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let pause_gate = matches!(point, OriginalFailurePoint::HistoryWorkerTransition)
        .then(|| Arc::new(FinishFaultPauseGate::default()));
    let worker_gate = matches!(point, OriginalFailurePoint::CandidateWorkerTransition)
        .then(|| Arc::new(FinishFaultReleaseGate::default()));
    let fault_registry = if let Some(gate) = &worker_gate {
        Some(Arc::new(FinishFaultRegistry::candidate_then_worker(
            Arc::clone(&store),
            Arc::clone(gate),
        )))
    } else {
        point.transition_fault().map(|reason| {
            let registry = FinishFaultRegistry::new(Arc::clone(&store), reason);
            Arc::new(match &pause_gate {
                Some(gate) => registry.with_pause_gate(Arc::clone(gate)),
                None => registry,
            })
        })
    };
    let registry: Arc<dyn GoalTaskRegistry> = match &fault_registry {
        Some(registry) => registry.clone(),
        None => store.clone(),
    };
    let runtime = GoalRuntime::new(Arc::clone(&registry));
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let snapshot = live.handle().snapshot();
    let settings = host_settings(&snapshot);
    let directory = TempDir::new().unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory.path(),
        )
        .unwrap(),
    );
    let factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            registry,
            Arc::clone(&tracker),
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::new(
                std::collections::HashMap::from([(
                    "configured".into(),
                    std::collections::HashMap::from([
                        ("model.input".into(), 1.0),
                        ("model.output".into(), 2.0),
                        ("model.cached_input".into(), 0.5),
                        ("model.cache_write".into(), 1.5),
                    ]),
                )]),
            ))),
        )
        .unwrap(),
    );
    let supervisor = GoalExecutionSupervisor::new(Arc::new(runtime.execution_engine(factory)));
    let events = Arc::new(Mutex::new(Vec::new()));
    let published = Arc::new(Notify::new());
    let parent_gate = matches!(
        point,
        OriginalFailurePoint::CancelledParent | OriginalFailurePoint::ParentFencedPending
    )
    .then(|| Arc::new(OriginalParentGate::default()));
    let driver = Arc::new(OriginalErrorDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        point,
        acquires: AtomicUsize::new(0),
        events: Arc::clone(&events),
        published: Arc::clone(&published),
        store: Arc::clone(&store),
        parent_gate: parent_gate.clone(),
        counts: Arc::new(OriginalOperationCounts::default()),
    });
    let mut started = supervisor
        .submit(
            &settings,
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the admitted task".into(),
            },
        )
        .await
        .unwrap();
    let task_id = match started.response() {
        GoalResponse::Started(projection) => projection.task_id.clone(),
        response => panic!("expected start response, got {response:?}"),
    };
    if matches!(point, OriginalFailurePoint::ResumeAppend) {
        let paused = supervisor
            .submit(
                &settings,
                ingress.clone(),
                driver.clone(),
                GoalCommand::Pause { immediate: false },
            )
            .await
            .unwrap();
        assert!(matches!(paused.response(), GoalResponse::Paused(_)));
        started = supervisor
            .submit(
                &settings,
                ingress.clone(),
                driver.clone(),
                GoalCommand::Resume {
                    response: Some("First line\nSecond line".into()),
                },
            )
            .await
            .unwrap();
    }
    let epoch = match started.response() {
        GoalResponse::Started(projection) | GoalResponse::Resumed(projection) => {
            assert_eq!(projection.task_id, task_id);
            projection.execution_epoch
        }
        response => panic!("expected admitted execution, got {response:?}"),
    };
    let scope =
        GoalExecutionScope::new(&task_id, ingress.session_key().durable_id(), epoch).unwrap();
    if matches!(point, OriginalFailurePoint::IncompleteBeforeExecution) {
        assert_eq!(
            store
                .admit_pending_operation(&task_id, scope.session_id(), epoch, "precheck-operation")
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
        assert_eq!(
            store
                .settle_pending_operation(
                    &task_id,
                    scope.session_id(),
                    epoch,
                    "precheck-operation",
                    GoalAccountingState::Missing
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
    }
    supervisor
        .launch_after_response(&settings_source, &mut started)
        .await
        .unwrap();
    let completion = if let Some(gate) = parent_gate {
        timeout(Duration::from_secs(5), gate.observed.notified())
            .await
            .expect("the admitted parent must reach its error gate");
        let mut control = supervisor
            .submit(
                &settings,
                ingress.clone(),
                driver.clone(),
                if matches!(point, OriginalFailurePoint::CancelledParent) {
                    GoalCommand::Cancel
                } else {
                    GoalCommand::Pause { immediate: true }
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            control.response(),
            GoalResponse::TransitionPending(_)
        ));
        gate.released.notify_one();
        let (completion, transition) = timeout(Duration::from_secs(5), async {
            tokio::join!(biased;
                supervisor.drain(&scope),
                supervisor.launch_after_response(&settings_source, &mut control),
            )
        })
        .await
        .expect("the fenced parent error must drain without a deadlock");
        transition.unwrap();
        completion
    } else if matches!(point, OriginalFailurePoint::ReacquisitionFailure) {
        timeout(Duration::from_secs(5), supervisor.drain(&scope))
            .await
            .expect("failed reacquisition must not strand the worker")
    } else if let Some(gate) = &pause_gate {
        timeout(Duration::from_secs(5), gate.observed.notified())
            .await
            .expect("the named worker terminal transition must reach the fault gate");
        assert_eq!(
            *events.lock().unwrap(),
            vec!["original_error"],
            "the held lease must deliver the history error before worker fallback"
        );
        let mut pause = supervisor
            .submit(
                &settings,
                ingress.clone(),
                driver.clone(),
                GoalCommand::Pause { immediate: true },
            )
            .await
            .unwrap();
        assert!(matches!(
            pause.response(),
            GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Pause)
        ));
        let (completion, transition) = timeout(Duration::from_secs(5), async {
            // Capture the worker completion before the command's drainer
            // can retire it. Both drainers exercise the same finalization.
            tokio::join!(biased;
                supervisor.drain(&scope),
                supervisor.launch_after_response(&settings_source, &mut pause),
            )
        })
        .await
        .expect("a pause-fenced error must settle without a drain deadlock");
        let Some(GoalResponse::Paused(paused)) = transition.unwrap() else {
            panic!("the Complete operator pause must remain the late response");
        };
        assert_eq!(paused.task_id, task_id);
        assert_eq!(paused.execution_epoch, epoch + 1);
        assert_eq!(paused.pause_reason, Some(GoalPauseReason::OperatorPaused));
        completion
    } else if let Some(gate) = &worker_gate {
        timeout(Duration::from_secs(5), gate.observed.notified())
            .await
            .expect("the second fault must reach the actual worker fallback");
        let current = store
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.id, task_id);
        assert_eq!(current.status, TaskStatus::Running);
        assert_eq!(current.execution_epoch, epoch);
        let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert!(events.lock().unwrap().is_empty());
        gate.released.notify_one();
        let completion = timeout(Duration::from_secs(5), supervisor.drain(&scope))
            .await
            .expect("drain must apply the third finish attempt without an operator pause");
        timeout(Duration::from_secs(5), published.notified())
            .await
            .expect("drain must publish the earliest candidate aggregate then failure notice");
        completion
    } else {
        timeout(Duration::from_secs(5), published.notified())
            .await
            .expect("the failed worker must publish its lifecycle notice");
        supervisor.drain(&scope).await
    };
    let session_id = ingress.session_key().durable_id();
    let task = store
        .current_goal_for_session(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.id, task_id);
    assert_eq!(
        task.status,
        if matches!(point, OriginalFailurePoint::CancelledParent) {
            TaskStatus::Cancelled
        } else if matches!(point, OriginalFailurePoint::HistoryWorkerTransition) {
            TaskStatus::Paused
        } else {
            TaskStatus::Failed
        }
    );
    assert_eq!(
        store
            .terminal_reason_for_session_goal(&task_id, &session_id)
            .await
            .unwrap()
            .as_deref(),
        if matches!(
            point,
            OriginalFailurePoint::CancelledParent | OriginalFailurePoint::HistoryWorkerTransition
        ) {
            None
        } else {
            Some(point.reason().0)
        }
    );
    if matches!(
        point,
        OriginalFailurePoint::HostCandidate
            | OriginalFailurePoint::CandidateTransition
            | OriginalFailurePoint::CandidateWorkerTransition
            | OriginalFailurePoint::AccountingVerdict
            | OriginalFailurePoint::HistoryWorkerTransition
    ) {
        assert_eq!(task.execution_epoch, epoch + 1);
    }
    let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
    assert_eq!(
        goal.accounting_state,
        if matches!(
            point,
            OriginalFailurePoint::IncompleteBeforeExecution
                | OriginalFailurePoint::AccountingVerdict
        ) {
            GoalAccountingState::Missing
        } else if matches!(point, OriginalFailurePoint::ParentFencedPending) {
            GoalAccountingState::OutcomeUnknown
        } else {
            GoalAccountingState::Complete
        }
    );
    assert!(goal.pending_call_id.is_none());
    assert!(goal.pending_call_epoch.is_none());
    if matches!(point, OriginalFailurePoint::HistoryWorkerTransition) {
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::OperatorPaused));
    }
    assert_eq!(
        tracker
            .get_summary_for_task(&task_id)
            .unwrap()
            .request_count,
        usize::from(matches!(point, OriginalFailurePoint::PricingVerdict))
    );
    if matches!(
        point,
        OriginalFailurePoint::HostCandidate
            | OriginalFailurePoint::CandidateTransition
            | OriginalFailurePoint::CandidateWorkerTransition
            | OriginalFailurePoint::AccountingVerdict
            | OriginalFailurePoint::HistoryWorkerTransition
    ) {
        let history_failure = matches!(point, OriginalFailurePoint::HistoryWorkerTransition);
        let missing = matches!(point, OriginalFailurePoint::AccountingVerdict);
        assert_eq!(driver.counts.history.load(Ordering::SeqCst), 1);
        assert_eq!(
            driver.counts.parent.load(Ordering::SeqCst),
            usize::from(!history_failure)
        );
        assert_eq!(
            driver.counts.append.load(Ordering::SeqCst),
            usize::from(!history_failure && !missing)
        );
        assert_eq!(driver.counts.committed.load(Ordering::SeqCst), 0);
        assert_eq!(driver.counts.verifier.load(Ordering::SeqCst), 0);
    }
    if matches!(point, OriginalFailurePoint::PricingVerdict) {
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(&task_id)
                .unwrap(),
            (7, 0.0, false)
        );
    }
    let expected_acquires = if matches!(point, OriginalFailurePoint::ReacquisitionFailure) {
        3
    } else if matches!(
        point,
        OriginalFailurePoint::HostParent
            | OriginalFailurePoint::HostVerifier
            | OriginalFailurePoint::HostCandidate
            | OriginalFailurePoint::HistoryWorkerTransition
            | OriginalFailurePoint::EmptyCandidate
            | OriginalFailurePoint::IncompleteBeforeExecution
            | OriginalFailurePoint::ProtocolVerdict
            | OriginalFailurePoint::AccountingVerdict
            | OriginalFailurePoint::PricingVerdict
            | OriginalFailurePoint::CancelledParent
    ) {
        1
    } else {
        2
    };
    assert_eq!(driver.acquires.load(Ordering::SeqCst), expected_acquires);
    let completion = completion.unwrap_err();
    let expected_completion = match point {
        OriginalFailurePoint::Acquisition | OriginalFailurePoint::ReacquisitionFailure => {
            "acquire Goal session execution lease: distinctive original execution failure"
        }
        OriginalFailurePoint::HostParent => {
            "Goal parent operation failed: distinctive original execution failure"
        }
        OriginalFailurePoint::HostVerifier => {
            "Goal verifier operation failed: distinctive original execution failure"
        }
        OriginalFailurePoint::HostCandidate => original_native_chain(point),
        OriginalFailurePoint::EmptyCandidate => "Goal parent returned an empty candidate",
        OriginalFailurePoint::HostParentTransition
        | OriginalFailurePoint::HostVerifierTransition
        | OriginalFailurePoint::CandidateTransition => original_native_chain(point),
        OriginalFailurePoint::HistoryWorkerTransition => {
            "failed to terminalize Goal executor failure: distinctive original Goal transition failure"
        }
        OriginalFailurePoint::CandidateWorkerTransition => {
            "failed to terminalize Goal executor failure: distinctive worker Goal transition failure"
        }
        OriginalFailurePoint::IncompleteBeforeExecution => {
            "Goal execution begins with incomplete accounting"
        }
        OriginalFailurePoint::ProtocolVerdict => {
            "Goal verifier response is invalid: verifier response is not strict JSON: expected ident at line 1 column 2"
        }
        OriginalFailurePoint::AccountingVerdict => MISSING_SETTLEMENT_ERROR,
        OriginalFailurePoint::PricingVerdict => {
            "Goal cost budget lacks complete pricing for actual provider usage"
        }
        OriginalFailurePoint::CancelledParent | OriginalFailurePoint::ParentFencedPending => {
            "Goal failure lost its execution fence: Goal parent operation failed: distinctive original execution failure"
        }
        _ => "distinctive original execution failure",
    };
    assert_eq!(format!("{completion:#}"), expected_completion);
    if let Some(registry) = fault_registry {
        assert_eq!(
            registry.failures(),
            if matches!(point, OriginalFailurePoint::CandidateWorkerTransition) {
                2
            } else {
                1
            },
            "each scripted transition must fail exactly once"
        );
        if matches!(point, OriginalFailurePoint::CandidateWorkerTransition) {
            assert_eq!(
                registry.attempts(),
                vec![
                    "candidate_persistence_failed",
                    "executor_failed",
                    "executor_failed"
                ]
            );
            assert_eq!(registry.applied(), 1);
        }
    }
    let expected_events = match point {
        OriginalFailurePoint::ResumeAppend => {
            vec!["resume_append", "original_error", "failure_notice"]
        }
        OriginalFailurePoint::HostParent
        | OriginalFailurePoint::HostVerifier
        | OriginalFailurePoint::AccountingVerdict => vec!["host_error", "failure_notice"],
        OriginalFailurePoint::EmptyCandidate
        | OriginalFailurePoint::ProtocolVerdict
        | OriginalFailurePoint::PricingVerdict => vec!["failure_notice"],
        OriginalFailurePoint::HostParentTransition
        | OriginalFailurePoint::HostVerifierTransition
        | OriginalFailurePoint::ParentFencedPending => {
            vec!["host_error", "original_error", "failure_notice"]
        }
        OriginalFailurePoint::ReacquisitionFailure => Vec::new(),
        OriginalFailurePoint::CancelledParent => vec!["host_error"],
        OriginalFailurePoint::HistoryWorkerTransition => vec!["original_error"],
        _ => vec!["original_error", "failure_notice"],
    };
    assert_eq!(*events.lock().unwrap(), expected_events,);
    let mut delivery_warnings = Vec::new();
    loop {
        match log_events.try_recv() {
            Ok(event) => {
                if event["attributes"]["task_id"] == task_id && event["severity_text"] == "WARN" {
                    delivery_warnings.push(event);
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(error) => panic!("canonical log capture lost evidence: {error}"),
        }
    }
    let expected_warning = match point {
        OriginalFailurePoint::HistoryDeliveryFailure => Some(("core_error_delivery", 1)),
        OriginalFailurePoint::ReacquisitionFailure => Some(("failure_notice_delivery", 2)),
        _ => None,
    };
    if let Some((phase, count)) = expected_warning {
        assert_eq!(delivery_warnings.len(), count);
        for event in delivery_warnings {
            let attributes = &event["attributes"];
            assert_eq!(attributes.as_object().unwrap().len(), 5);
            assert_eq!(attributes["task_id"], task_id);
            assert_eq!(attributes["execution_epoch"], epoch);
            assert_eq!(attributes["phase"], phase);
            // The canonical capture layer adds source navigation metadata;
            // the failure producer adds no raw error, prompt or payload.
            assert_eq!(
                attributes["_file"],
                "crates/zeroclaw-goal/src/control/goal_execution.rs"
            );
            assert!(attributes["_line"].as_u64().is_some_and(|line| line > 0));
        }
    }
}

#[tokio::test]
async fn original_history_error_precedes_failure_notice() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::History).await;
}

#[tokio::test]
async fn original_acquisition_error_precedes_failure_notice() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::Acquisition)
        .await;
}

#[tokio::test]
async fn original_resume_append_error_precedes_failure_notice() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::ResumeAppend)
        .await;
}

#[tokio::test]
async fn original_error_delivery_failure_still_publishes_notice_without_retry_on_drain() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::HistoryDeliveryFailure,
    )
    .await;
}

#[tokio::test]
async fn original_host_parent_error_is_not_handed_again() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::HostParent).await;
}

#[tokio::test]
async fn original_host_verifier_error_is_not_handed_again() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::HostVerifier)
        .await;
}

#[tokio::test]
async fn original_private_candidate_error_precedes_candidate_failure_notice() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::HostCandidate)
        .await;
}

#[tokio::test]
async fn original_private_candidate_transition_preserves_append_error() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::CandidateTransition,
    )
    .await;
}

#[tokio::test]
async fn original_empty_candidate_verdict_is_not_handed_as_core_error() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::EmptyCandidate)
        .await;
}

#[tokio::test]
async fn original_host_parent_transition_error_is_handed_after_host_error() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::HostParentTransition,
    )
    .await;
}

#[tokio::test]
async fn original_host_verifier_transition_error_is_handed_after_host_error() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::HostVerifierTransition,
    )
    .await;
}

#[tokio::test]
async fn original_history_error_wins_over_later_worker_transition_failure() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::HistoryWorkerTransition,
    )
    .await;
}

#[tokio::test]
async fn original_candidate_error_wins_over_reached_worker_transition_failure() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::CandidateWorkerTransition,
    )
    .await;
}

#[tokio::test]
async fn original_incomplete_accounting_precheck_error_precedes_failure_notice() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::IncompleteBeforeExecution,
    )
    .await;
}

#[tokio::test]
async fn original_reacquisition_failure_publishes_nothing_and_logs_delivery_failures() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::ReacquisitionFailure,
    )
    .await;
}

#[tokio::test]
async fn original_protocol_verdict_is_not_handed_as_core_error() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::ProtocolVerdict)
        .await;
}

#[tokio::test]
async fn original_accounting_verdict_is_not_handed_as_core_error() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::AccountingVerdict,
    )
    .await;
}

#[tokio::test]
async fn original_pricing_verdict_preserves_known_usage_without_core_error_handoff() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::PricingVerdict)
        .await;
}

#[tokio::test]
async fn original_cancelled_epoch_error_publishes_no_handoff_or_failure_notice() {
    assert_original_execution_error_precedes_failure_notice(OriginalFailurePoint::CancelledParent)
        .await;
}

#[tokio::test]
async fn original_fence_lost_error_precedes_outcome_unknown_notice() {
    assert_original_execution_error_precedes_failure_notice(
        OriginalFailurePoint::ParentFencedPending,
    )
    .await;
}

// Separate ingress cases ensure an expected Matrix failure cannot prevent
// execution of the equivalent ZeroCode control-plane fixture.
macro_rules! original_error_zerocode_case {
    ($name:ident, $point:ident) => {
        #[tokio::test]
        async fn $name() {
            assert_original_execution_error_for_ingress(
                OriginalFailurePoint::$point,
                zerocode_ingress("main"),
            )
            .await;
        }
    };
}

original_error_zerocode_case!(original_zerocode_history_error, History);
original_error_zerocode_case!(original_zerocode_acquisition_error, Acquisition);
original_error_zerocode_case!(original_zerocode_resume_append_error, ResumeAppend);
original_error_zerocode_case!(original_zerocode_delivery_failure, HistoryDeliveryFailure);
original_error_zerocode_case!(original_zerocode_host_parent_error, HostParent);
original_error_zerocode_case!(original_zerocode_host_verifier_error, HostVerifier);
original_error_zerocode_case!(original_zerocode_private_candidate_error, HostCandidate);
original_error_zerocode_case!(
    original_zerocode_private_candidate_transition,
    CandidateTransition
);
original_error_zerocode_case!(original_zerocode_empty_candidate_verdict, EmptyCandidate);
original_error_zerocode_case!(
    original_zerocode_parent_transition_error,
    HostParentTransition
);
original_error_zerocode_case!(
    original_zerocode_verifier_transition_error,
    HostVerifierTransition
);
original_error_zerocode_case!(
    original_zerocode_earliest_history_error,
    HistoryWorkerTransition
);
original_error_zerocode_case!(
    original_zerocode_candidate_error_wins_over_reached_worker_transition_failure,
    CandidateWorkerTransition
);
original_error_zerocode_case!(
    original_zerocode_accounting_precheck_error,
    IncompleteBeforeExecution
);
original_error_zerocode_case!(
    original_zerocode_reacquisition_failure,
    ReacquisitionFailure
);
original_error_zerocode_case!(original_zerocode_protocol_verdict, ProtocolVerdict);
original_error_zerocode_case!(original_zerocode_accounting_verdict, AccountingVerdict);
original_error_zerocode_case!(original_zerocode_pricing_verdict, PricingVerdict);
original_error_zerocode_case!(original_zerocode_cancelled_epoch_error, CancelledParent);
original_error_zerocode_case!(original_zerocode_fence_lost_error, ParentFencedPending);

async fn assert_public_run_preserves_original_errors(ingress: GoalIngressContext) {
    for (point, expected, transition_error) in [
        (
            OriginalFailurePoint::History,
            "distinctive original execution failure",
            false,
        ),
        (
            OriginalFailurePoint::Acquisition,
            "acquire Goal session execution lease: distinctive original execution failure",
            false,
        ),
        (
            OriginalFailurePoint::HostParent,
            "Goal parent operation failed: distinctive original execution failure",
            false,
        ),
        (
            OriginalFailurePoint::HostVerifier,
            "Goal verifier operation failed: distinctive original execution failure",
            false,
        ),
        (
            OriginalFailurePoint::HostCandidate,
            "distinctive original execution failure",
            false,
        ),
        (
            OriginalFailurePoint::HostParentTransition,
            "distinctive original Goal transition failure: Goal parent operation failed: distinctive original execution failure",
            true,
        ),
        (
            OriginalFailurePoint::HostVerifierTransition,
            "distinctive original Goal transition failure: Goal verifier operation failed: distinctive original execution failure",
            true,
        ),
        (
            OriginalFailurePoint::CandidateTransition,
            "distinctive original Goal transition failure: distinctive original execution failure",
            true,
        ),
        (
            OriginalFailurePoint::AccountingVerdict,
            MISSING_SETTLEMENT_ERROR,
            false,
        ),
        (
            OriginalFailurePoint::CancelledParent,
            "Goal failure lost its execution fence: Goal parent operation failed: distinctive original execution failure",
            false,
        ),
    ] {
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let registry: Arc<dyn GoalTaskRegistry> = match point.transition_fault() {
            Some(reason) => Arc::new(FinishFaultRegistry::new(Arc::clone(&store), reason)),
            None => store.clone(),
        };
        let runtime = GoalRuntime::new(Arc::clone(&registry));
        let live = LiveConfig::new(valid_goal_config(true));
        let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
        let snapshot = live.handle().snapshot();
        let settings = host_settings(&snapshot);
        let events = Arc::new(Mutex::new(Vec::new()));
        let parent_gate = matches!(point, OriginalFailurePoint::CancelledParent)
            .then(|| Arc::new(OriginalParentGate::default()));
        let driver = Arc::new(OriginalErrorDriver {
            binding: GoalSessionBinding::new(ingress.session_key().clone()),
            point,
            acquires: AtomicUsize::new(0),
            events: Arc::clone(&events),
            published: Arc::new(Notify::new()),
            store: Arc::clone(&store),
            parent_gate: parent_gate.clone(),
            counts: Arc::new(OriginalOperationCounts::default()),
        });
        let request = runtime
            .submit(
                &settings,
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "finish the admitted task".into(),
                },
            )
            .await
            .unwrap()
            .into_parts()
            .1
            .unwrap();
        let directory = TempDir::new().unwrap();
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: false,
                    ..Default::default()
                },
                directory.path(),
            )
            .unwrap(),
        );
        let prices = if matches!(point, OriginalFailurePoint::AccountingVerdict) {
            std::collections::HashMap::from([(
                "configured".into(),
                std::collections::HashMap::from([
                    ("model.input".into(), 1.0),
                    ("model.output".into(), 2.0),
                    ("model.cached_input".into(), 0.5),
                    ("model.cache_write".into(), 1.5),
                ]),
            )])
        } else {
            std::collections::HashMap::new()
        };
        let engine = runtime.execution_engine(Arc::new(
            RuntimeGoalOperationAccountingFactory::new(
                registry,
                tracker,
                "main",
                Arc::new(ModelProviderGoalUsagePricer::new(Arc::new(prices))),
            )
            .unwrap(),
        ));
        let scope = request.scope().clone();
        let run = engine.run(&settings_source, request, GoalParentTurnCheckpoint::new());
        let result = if let Some(gate) = parent_gate {
            timeout(Duration::from_secs(5), async {
                let (result, ()) = tokio::join!(run, async {
                    gate.observed.notified().await;
                    assert_eq!(
                        store
                            .finish_session_goal(
                                scope.task_id(),
                                scope.session_id(),
                                scope.execution_epoch(),
                                TaskStatus::Cancelled,
                                Some("operator_cancel".into()),
                            )
                            .await
                            .unwrap(),
                        GoalTransitionResult::Applied
                    );
                    gate.released.notify_one();
                });
                result
            })
            .await
            .expect("public run must preserve the original through the winning cancel fence")
        } else {
            run.await
        };
        let error = result.unwrap_err();
        assert_eq!(format!("{error:#}"), expected);
        assert!(error.downcast_ref::<OriginalTransitionFailure>().is_none());
        assert!(error.downcast_ref::<WorkerTransitionFailure>().is_none());
        let missing = matches!(point, OriginalFailurePoint::AccountingVerdict);
        assert_eq!(
            error.downcast_ref::<OriginalExecutionFailure>().is_some(),
            !missing
        );
        if missing {
            let failure = error.downcast_ref::<GoalAccountingFailure>().unwrap();
            assert_eq!(failure.accounting_state(), GoalAccountingState::Missing);
            assert!(!failure.has_pending_operation());
        }
        assert_eq!(
            format!("{error:#}")
                .matches("distinctive original execution failure")
                .count(),
            usize::from(!missing)
        );
        assert_eq!(
            format!("{error:#}")
                .matches("distinctive original Goal transition failure")
                .count(),
            usize::from(transition_error)
        );
        assert_eq!(driver.acquires.load(Ordering::SeqCst), 1);
        let expected_events = if matches!(
            point,
            OriginalFailurePoint::History
                | OriginalFailurePoint::Acquisition
                | OriginalFailurePoint::HostCandidate
                | OriginalFailurePoint::CandidateTransition
        ) {
            Vec::new()
        } else {
            vec!["host_error"]
        };
        assert_eq!(
            *events.lock().unwrap(),
            expected_events,
            "the public run path must not acquire a supervisor publication latch"
        );
    }
}

#[tokio::test]
async fn original_public_run_keeps_error_identity_and_completion_text() {
    assert_public_run_preserves_original_errors(matrix_ingress()).await;
}

#[tokio::test]
async fn original_zerocode_public_run_keeps_error_identity_and_completion_text() {
    assert_public_run_preserves_original_errors(zerocode_ingress("main")).await;
}

struct LaunchRebindFailureDriver {
    inner: Arc<RecordingDriver>,
    refuse_bind: AtomicBool,
    notices: Arc<Mutex<Vec<zeroclaw_goal::GoalExecutionNotice>>>,
    parent_calls: Arc<AtomicUsize>,
}

struct LaunchRebindFailureLease {
    inner: RecordingExecutionLease,
    notices: Arc<Mutex<Vec<zeroclaw_goal::GoalExecutionNotice>>>,
    parent_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl GoalSessionExecutionLease for LaunchRebindFailureLease {
    fn session_key(&self) -> &GoalSessionKey {
        self.inner.session_key()
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        self.inner.canonical_history()
    }

    async fn run_parent_turn(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        self.parent_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.run_parent_turn(operation, turn).await
    }

    async fn run_verifier(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        self.inner.run_verifier(operation, turn).await
    }

    async fn append_parent_candidate(&mut self, candidate: String) -> anyhow::Result<()> {
        self.inner.append_parent_candidate(candidate).await
    }

    async fn publish_goal_notice(
        &mut self,
        notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        self.notices.lock().unwrap().push(notice);
        Ok(())
    }
}

#[async_trait]
impl GoalSessionDriver for LaunchRebindFailureDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.inner.session_key()
    }

    async fn bind(&self, ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        if self.refuse_bind.load(Ordering::SeqCst) {
            anyhow::bail!("session binding revoked after acknowledgement");
        }
        self.inner.bind(ingress).await
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        self.inner.execution_acquires.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(LaunchRebindFailureLease {
            inner: RecordingExecutionLease {
                session_key: self.inner.session_key().clone(),
                delivered: Arc::new(AtomicUsize::new(0)),
            },
            notices: Arc::clone(&self.notices),
            parent_calls: Arc::clone(&self.parent_calls),
        }))
    }
}

struct RecordingExecutionLease {
    session_key: GoalSessionKey,
    delivered: Arc<AtomicUsize>,
}

#[async_trait]
impl GoalSessionExecutionLease for RecordingExecutionLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _scope: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        Ok(GoalParentTurnResult::from_continuation_prefix(
            format!("parent:{}", turn.objective),
            turn.working_history,
        ))
    }

    async fn run_verifier(
        &mut self,
        _scope: &GoalOperationScope,
        turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        Ok(GoalVerifierResult {
            response: format!("verifier:{}", turn.candidate),
            attribution: None,
        })
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        self.delivered.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn publish_goal_notice(
        &mut self,
        _notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

struct ExecutionDriver {
    binding: GoalSessionBinding,
    execution_acquires: AtomicUsize,
    delivered: Arc<AtomicUsize>,
}

struct TranscriptExecutionLease {
    session_key: GoalSessionKey,
    delivered: Arc<AtomicUsize>,
    continue_once: bool,
    parent_histories: Arc<Mutex<Vec<Vec<zeroclaw_api::model_provider::ChatMessage>>>>,
    parent_turn_kinds: Arc<Mutex<Vec<GoalParentTurnKind>>>,
}

#[async_trait]
impl GoalSessionExecutionLease for TranscriptExecutionLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _operation: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        self.parent_histories
            .lock()
            .unwrap()
            .push(turn.working_history.clone());
        self.parent_turn_kinds.lock().unwrap().push(turn.kind);
        let candidate = format!("parent:{}", turn.objective);
        let mut completed_history = turn.working_history;
        completed_history.push(zeroclaw_api::model_provider::ChatMessage::assistant(
            candidate.clone(),
        ));
        GoalParentTurnResult::from_completed_agent_turn(candidate, completed_history)
    }

    async fn run_verifier(
        &mut self,
        _operation: &GoalOperationScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        if self.continue_once {
            self.continue_once = false;
            Ok(GoalVerifierResult {
                response:
                    r#"{"decision":"continue","reason":"add the missing detail","blockers":[]}"#
                        .to_owned(),
                attribution: None,
            })
        } else {
            Ok(GoalVerifierResult {
                response: r#"{"decision":"complete","reason":"candidate satisfies the objective","blockers":[]}"#
                    .to_owned(),
                attribution: None,
            })
        }
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        self.delivered.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn publish_goal_notice(
        &mut self,
        _notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

struct TranscriptExecutionDriver {
    binding: GoalSessionBinding,
    delivered: Arc<AtomicUsize>,
    parent_histories: Arc<Mutex<Vec<Vec<zeroclaw_api::model_provider::ChatMessage>>>>,
    parent_turn_kinds: Arc<Mutex<Vec<GoalParentTurnKind>>>,
}

#[async_trait]
impl GoalSessionDriver for TranscriptExecutionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        Ok(Box::new(TranscriptExecutionLease {
            session_key: self.binding.session_key().clone(),
            delivered: Arc::clone(&self.delivered),
            continue_once: true,
            parent_histories: Arc::clone(&self.parent_histories),
            parent_turn_kinds: Arc::clone(&self.parent_turn_kinds),
        }))
    }
}

struct PausingExecutionLease {
    session_key: GoalSessionKey,
    parent_started: Arc<Notify>,
    release_parent: Arc<Notify>,
    verifier_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl GoalSessionExecutionLease for PausingExecutionLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _operation: &GoalOperationScope,
        _turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        self.parent_started.notify_one();
        self.release_parent.notified().await;
        Ok(GoalParentTurnResult::from_continuation_prefix(
            "candidate that settled before pause".to_owned(),
            Vec::new(),
        ))
    }

    async fn run_verifier(
        &mut self,
        _operation: &GoalOperationScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        self.verifier_calls.fetch_add(1, Ordering::SeqCst);
        Ok(GoalVerifierResult {
            response: r#"{"decision":"complete","reason":"done","blockers":[]}"#.to_owned(),
            attribution: None,
        })
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        Ok(())
    }

    async fn publish_goal_notice(
        &mut self,
        _notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

struct PausingExecutionDriver {
    binding: GoalSessionBinding,
    execution_acquires: AtomicUsize,
    parent_started: Arc<Notify>,
    release_parent: Arc<Notify>,
    verifier_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl GoalSessionDriver for PausingExecutionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        self.execution_acquires.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(PausingExecutionLease {
            session_key: self.binding.session_key().clone(),
            parent_started: Arc::clone(&self.parent_started),
            release_parent: Arc::clone(&self.release_parent),
            verifier_calls: Arc::clone(&self.verifier_calls),
        }))
    }
}

const INVALID_LIVE_VERIFIER_ERROR: &str = "Goal configuration does not admit execution: [dangling_reference] goal.verifier.model_provider = \"openai.missing\" but no matching providers.models entry is configured (goal.verifier.model_provider)";

// Observe the concrete pausing fixture without treating publication custody as
// execution. The failed task's lease must never enter any execution method.
struct LiveConfigObservingDriver {
    inner: Arc<PausingExecutionDriver>,
    store: Arc<SqliteTaskStore>,
    acquisitions: Mutex<Vec<(String, i64, TaskStatus)>>,
    calls: Arc<OriginalOperationCounts>,
    events: Arc<Mutex<Vec<&'static str>>>,
    published: Arc<Notify>,
}

struct LiveConfigObservingLease {
    inner: Box<dyn GoalSessionExecutionLease>,
    publication_only: bool,
    calls: Arc<OriginalOperationCounts>,
    events: Arc<Mutex<Vec<&'static str>>>,
    published: Arc<Notify>,
}

#[async_trait]
impl GoalSessionDriver for LiveConfigObservingDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.inner.session_key()
    }

    async fn bind(&self, ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        self.inner.bind(ingress).await
    }

    async fn acquire_execution(
        &self,
        ingress: &GoalIngressContext,
        scope: &GoalExecutionScope,
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        let task = self
            .store
            .current_goal_for_session(scope.session_id())
            .await?
            .unwrap();
        assert_eq!(task.id, scope.task_id());
        assert!(matches!(
            task.status,
            TaskStatus::Failed | TaskStatus::Running
        ));
        let publication_only = task.status == TaskStatus::Failed;
        assert_eq!(
            task.execution_epoch,
            scope.execution_epoch() + i64::from(publication_only)
        );
        self.acquisitions
            .lock()
            .unwrap()
            .push((task.id, scope.execution_epoch(), task.status));
        Ok(Box::new(LiveConfigObservingLease {
            inner: self
                .inner
                .acquire_execution(ingress, scope, checkpoint)
                .await?,
            publication_only,
            calls: Arc::clone(&self.calls),
            events: Arc::clone(&self.events),
            published: Arc::clone(&self.published),
        }))
    }
}

#[async_trait]
impl GoalSessionExecutionLease for LiveConfigObservingLease {
    fn session_key(&self) -> &GoalSessionKey {
        self.inner.session_key()
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<ChatMessage>> {
        assert!(!self.publication_only);
        self.calls.history.fetch_add(1, Ordering::SeqCst);
        self.inner.canonical_history()
    }

    async fn append_resume_response(&mut self, response: String) -> anyhow::Result<()> {
        assert!(!self.publication_only);
        self.inner.append_resume_response(response).await
    }

    async fn run_parent_turn(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        assert!(!self.publication_only);
        self.calls.parent.fetch_add(1, Ordering::SeqCst);
        self.inner.run_parent_turn(operation, turn).await
    }

    async fn append_parent_candidate(&mut self, candidate: String) -> anyhow::Result<()> {
        assert!(!self.publication_only);
        self.calls.append.fetch_add(1, Ordering::SeqCst);
        self.inner.append_parent_candidate(candidate).await
    }

    async fn run_verifier(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        assert!(!self.publication_only);
        self.calls.verifier.fetch_add(1, Ordering::SeqCst);
        self.inner.run_verifier(operation, turn).await
    }

    async fn publish_goal_core_error(&mut self, error: &anyhow::Error) -> anyhow::Result<()> {
        assert!(self.publication_only);
        let native = error.downcast_ref::<ConfigApiError>().unwrap();
        assert_eq!(native.code, ConfigApiCode::DanglingReference);
        assert_eq!(native.path.as_deref(), Some("goal.verifier.model_provider"));
        assert_eq!(native.op_index, None);
        assert_eq!(
            native.message,
            "goal.verifier.model_provider = \"openai.missing\" but no matching providers.models entry is configured"
        );
        assert_eq!(format!("{error:#}"), INVALID_LIVE_VERIFIER_ERROR);
        assert_eq!(*self.events.lock().unwrap(), Vec::<&str>::new());
        self.events.lock().unwrap().push("original_config_error");
        Ok(())
    }

    async fn publish_goal_notice(
        &mut self,
        notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        assert!(self.publication_only);
        assert_eq!(
            notice,
            zeroclaw_goal::GoalExecutionNotice::Failed(
                zeroclaw_goal::GoalFailureReason::ExecutorFailed
            )
        );
        assert_eq!(*self.events.lock().unwrap(), vec!["original_config_error"]);
        self.events.lock().unwrap().push("failure_notice");
        self.published.notify_one();
        Ok(())
    }
}

struct UserInputExecutionDriver {
    binding: GoalSessionBinding,
    events: Arc<Mutex<Vec<&'static str>>>,
    candidates: Arc<Mutex<Vec<String>>>,
}

struct UserInputExecutionLease {
    session_key: GoalSessionKey,
    events: Arc<Mutex<Vec<&'static str>>>,
    candidates: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl GoalSessionExecutionLease for UserInputExecutionLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _scope: &GoalOperationScope,
        _turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        self.events.lock().unwrap().push("parent");
        Ok(GoalParentTurnResult::from_continuation_prefix(
            "Which option should I use?".to_owned(),
            Vec::new(),
        ))
    }

    async fn run_verifier(
        &mut self,
        _scope: &GoalOperationScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        self.events.lock().unwrap().push("verifier");
        Ok(GoalVerifierResult {
            response: r#"{"decision":"blocked","reason":"A user choice is required","blockers":[{"kind":"needs_user_input","message":"Which option should I use?"}]}"#.to_owned(),
            attribution: None,
        })
    }

    async fn append_parent_candidate(&mut self, candidate: String) -> anyhow::Result<()> {
        self.events.lock().unwrap().push("candidate");
        self.candidates.lock().unwrap().push(candidate);
        Ok(())
    }

    async fn publish_goal_notice(
        &mut self,
        notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        if notice == zeroclaw_goal::GoalExecutionNotice::PausedForBlocker {
            self.events.lock().unwrap().push("paused");
        }
        Ok(())
    }
}

#[async_trait]
impl GoalSessionDriver for UserInputExecutionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        Ok(Box::new(UserInputExecutionLease {
            session_key: self.binding.session_key().clone(),
            events: Arc::clone(&self.events),
            candidates: Arc::clone(&self.candidates),
        }))
    }
}

#[async_trait]
impl GoalSessionDriver for ExecutionDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        self.execution_acquires.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(RecordingExecutionLease {
            session_key: self.binding.session_key().clone(),
            delivered: self.delivered.clone(),
        }))
    }
}

struct WrongExecutionLeaseDriver {
    binding: GoalSessionBinding,
    returned_session_key: GoalSessionKey,
    execution_acquires: AtomicUsize,
}

#[async_trait]
impl GoalSessionDriver for WrongExecutionLeaseDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        self.execution_acquires.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(RecordingExecutionLease {
            session_key: self.returned_session_key.clone(),
            delivered: Arc::new(AtomicUsize::new(0)),
        }))
    }
}

struct WrongBindingDriver {
    advertised: GoalSessionKey,
    returned_binding: GoalSessionBinding,
    binds: AtomicUsize,
}

#[async_trait]
impl GoalSessionDriver for WrongBindingDriver {
    fn session_key(&self) -> &GoalSessionKey {
        &self.advertised
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        self.binds.fetch_add(1, Ordering::SeqCst);
        Ok(GoalSessionLease::new(self.returned_binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        anyhow::bail!("wrong binding driver has no execution lease")
    }
}

#[async_trait]
impl GoalSessionDriver for RecordingDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        self.binds.fetch_add(1, Ordering::SeqCst);
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &zeroclaw_goal::GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn zeroclaw_goal::GoalSessionExecutionLease>> {
        self.execution_acquires.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("recording driver has no execution lease")
    }
}

struct LeaseDriver {
    binding: GoalSessionBinding,
    available_for_reconnect: Arc<AtomicUsize>,
}

impl LeaseDriver {
    fn reconnect_is_allowed(&self) -> bool {
        self.available_for_reconnect.load(Ordering::SeqCst) == 1
    }

    fn acquire_reconnect_lease(&self) -> anyhow::Result<ReconnectLease> {
        if self
            .available_for_reconnect
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            anyhow::bail!("session replacement is already in progress");
        }
        Ok(ReconnectLease {
            session_key: self.binding.session_key().clone(),
            available_for_reconnect: self.available_for_reconnect.clone(),
        })
    }
}

struct ReconnectLease {
    session_key: GoalSessionKey,
    available_for_reconnect: Arc<AtomicUsize>,
}

impl Drop for ReconnectLease {
    fn drop(&mut self) {
        self.available_for_reconnect.store(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl GoalSessionExecutionLease for ReconnectLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _operation: &GoalOperationScope,
        _turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        anyhow::bail!("lease execution test does not run parent turns")
    }

    async fn run_verifier(
        &mut self,
        _operation: &GoalOperationScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        anyhow::bail!("lease execution test does not run verifier turns")
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        anyhow::bail!("lease execution test does not deliver candidates")
    }

    async fn publish_goal_notice(
        &mut self,
        _notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        anyhow::bail!("lease execution test does not publish Goal notices")
    }
}

#[async_trait]
impl GoalSessionDriver for LeaseDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(
            self.binding.clone(),
            self.acquire_reconnect_lease()?,
        ))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &zeroclaw_goal::GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn zeroclaw_goal::GoalSessionExecutionLease>> {
        Ok(Box::new(self.acquire_reconnect_lease()?))
    }
}

struct LeaseObservingRegistry {
    task: TaskRecord,
    goal: GoalTaskRecord,
    reconnect_available: Arc<AtomicUsize>,
    pause_observed: AtomicUsize,
    replacement_after_missing_goal: Option<TaskRecord>,
    missing_goal_observed: AtomicBool,
    task_after_goal_read: Option<TaskRecord>,
    goal_after_goal_read: Option<GoalTaskRecord>,
    goal_read_observed: AtomicBool,
}

impl LeaseObservingRegistry {
    fn running_for(ingress: &GoalIngressContext, reconnect_available: Arc<AtomicUsize>) -> Self {
        let task_id = "goal-under-lease".to_owned();
        Self {
            task: TaskRecord {
                id: task_id.clone(),
                kind: zeroclaw_runtime::control_plane::TaskKind::Goal,
                agent: ingress.agent().to_owned(),
                status: TaskStatus::Running,
                owner_pid: 42,
                owner_boot_id: "test-boot".to_owned(),
                heartbeat_at: None,
                depth: 0,
                parent_id: None,
                originator_route: Some(ingress.route().to_owned()),
                originator_chain: Vec::new(),
                delivered: false,
                idem_key: None,
                principal_id: Some("@alice:example.org".to_owned()),
                session_id: Some(ingress.session_key().durable_id()),
                execution_epoch: 1,
                started_at: "2026-09-02T00:00:00Z".to_owned(),
                finished_at: None,
            },
            goal: GoalTaskRecord {
                task_id,
                ..GoalTaskRecord::default()
            },
            reconnect_available,
            pause_observed: AtomicUsize::new(0),
            replacement_after_missing_goal: None,
            missing_goal_observed: AtomicBool::new(false),
            task_after_goal_read: None,
            goal_after_goal_read: None,
            goal_read_observed: AtomicBool::new(false),
        }
    }

    fn replaced_before_goal_read(ingress: &GoalIngressContext) -> Self {
        let reconnect_available = Arc::new(AtomicUsize::new(1));
        let mut registry = Self::running_for(ingress, reconnect_available);
        let mut replacement = registry.task.clone();
        replacement.id = "replacement-goal".to_owned();
        registry.replacement_after_missing_goal = Some(replacement);
        registry
    }

    fn paused_with_change_after_goal_read(
        ingress: &GoalIngressContext,
        changed_status: TaskStatus,
        changed_epoch: i64,
    ) -> Self {
        let mut registry = Self::running_for(ingress, Arc::new(AtomicUsize::new(1)));
        registry.task.status = TaskStatus::Paused;
        registry.goal.pause_reason = Some(GoalPauseReason::OperatorPaused);
        registry.goal.pause_description = Some("paused by operator".to_owned());
        let mut changed_task = registry.task.clone();
        changed_task.status = changed_status;
        changed_task.execution_epoch = changed_epoch;
        if changed_status == TaskStatus::Running {
            let mut resumed_goal = registry.goal.clone();
            resumed_goal.pause_reason = None;
            resumed_goal.pause_description = None;
            resumed_goal.blockers.clear();
            registry.goal_after_goal_read = Some(resumed_goal);
        }
        registry.task_after_goal_read = Some(changed_task);
        registry
    }

    fn assert_lease_is_held(&self) {
        assert_eq!(
            self.reconnect_available.load(Ordering::SeqCst),
            0,
            "the session lease must remain held during the durable transition"
        );
    }
}

#[async_trait]
impl GoalTaskRegistry for LeaseObservingRegistry {
    async fn latest_active_goal_for_agent(
        &self,
        _agent: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        panic!("unexpected legacy agent lookup")
    }

    async fn latest_active_goal_for_context(
        &self,
        _agent: &str,
        _originator_route: Option<&str>,
        _principal_id: Option<&str>,
    ) -> anyhow::Result<Option<TaskRecord>> {
        panic!("unexpected legacy context lookup")
    }

    async fn latest_active_goal_id_for_context(
        &self,
        _agent: &str,
        _originator_route: Option<&str>,
        _principal_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        panic!("unexpected legacy context-id lookup")
    }

    async fn get_goal_task(&self, task_id: &str) -> anyhow::Result<Option<GoalTaskRecord>> {
        assert_eq!(task_id, self.task.id);
        if self.replacement_after_missing_goal.is_some() {
            self.missing_goal_observed.store(true, Ordering::SeqCst);
            return Ok(None);
        }
        self.goal_read_observed.store(true, Ordering::SeqCst);
        Ok(Some(
            self.goal_after_goal_read
                .as_ref()
                .cloned()
                .unwrap_or_else(|| self.goal.clone()),
        ))
    }

    async fn update_goal_limits(
        &self,
        _task_id: &str,
        _token_limit: Option<u64>,
        _cost_limit_usd: Option<f64>,
    ) -> anyhow::Result<()> {
        panic!("unexpected legacy limits update")
    }

    async fn update_goal_pause(
        &self,
        _task_id: &str,
        _pause: Option<GoalPauseState>,
    ) -> anyhow::Result<()> {
        panic!("unexpected legacy pause update")
    }

    async fn set_continuation_context(
        &self,
        _task_id: &str,
        _context: Option<TaskContinuationContext>,
    ) -> anyhow::Result<()> {
        panic!("unexpected continuation update")
    }

    async fn get_continuation_context(
        &self,
        _task_id: &str,
    ) -> anyhow::Result<Option<TaskContinuationContext>> {
        panic!("unexpected continuation lookup")
    }

    async fn current_goal_for_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        assert_eq!(self.task.session_id.as_deref(), Some(session_id));
        Ok(Some(if self.goal_read_observed.load(Ordering::SeqCst) {
            self.task_after_goal_read
                .as_ref()
                .cloned()
                .unwrap_or_else(|| self.task.clone())
        } else {
            self.replacement_after_missing_goal
                .as_ref()
                .filter(|_| self.missing_goal_observed.load(Ordering::SeqCst))
                .cloned()
                .unwrap_or_else(|| self.task.clone())
        }))
    }

    async fn terminal_reason_for_session_goal(
        &self,
        task_id: &str,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        assert_eq!(task_id, self.task.id);
        assert_eq!(self.task.session_id.as_deref(), Some(session_id));
        Ok(None)
    }

    async fn create_or_replace_session_goal(
        &self,
        _task: TaskRecord,
        _goal: GoalTaskRecord,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected create-or-replace")
    }

    async fn pause_session_goal(
        &self,
        task_id: &str,
        session_id: &str,
        expected_epoch: i64,
        _pause: GoalPauseState,
    ) -> anyhow::Result<GoalTransitionResult> {
        assert_eq!(task_id, self.task.id);
        assert_eq!(self.task.session_id.as_deref(), Some(session_id));
        assert_eq!(expected_epoch, self.task.execution_epoch);
        self.assert_lease_is_held();
        self.pause_observed.fetch_add(1, Ordering::SeqCst);
        Ok(GoalTransitionResult::Applied)
    }

    async fn resume_session_goal(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _owner_pid: u32,
        _owner_boot_id: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected resume")
    }

    async fn finish_session_goal(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _status: TaskStatus,
        _error: Option<String>,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected finish")
    }

    async fn admit_pending_operation(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _operation_id: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected operation admission")
    }

    async fn settle_pending_operation(
        &self,
        _task_id: &str,
        _session_id: &str,
        _admitted_epoch: i64,
        _operation_id: &str,
        _accounting_state: GoalAccountingState,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected operation settlement")
    }

    async fn update_session_goal_limits(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
        _token_limit: Option<u64>,
        _cost_limit_usd: Option<f64>,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected session limits update")
    }

    async fn delete_session_goal(
        &self,
        _task_id: &str,
        _session_id: &str,
        _expected_epoch: i64,
    ) -> anyhow::Result<GoalTransitionResult> {
        panic!("unexpected goal deletion")
    }
}

fn matrix_ingress() -> GoalIngressContext {
    GoalIngressContext::trusted(
        GoalSessionKey::matrix("matrix_room__room_example_org__alice_example_org").unwrap(),
        "main",
        "matrix:primary",
        GoalIngressPrincipal::Matrix {
            raw_mxid: "@alice:example.org".into(),
        },
    )
    .unwrap()
}

fn zerocode_ingress(agent: &str) -> GoalIngressContext {
    GoalIngressContext::trusted(
        GoalSessionKey::zero_code("same-live-session").unwrap(),
        agent,
        "zerocode:local",
        GoalIngressPrincipal::ZeroCode {
            tui_id: "current-tui-connection".into(),
        },
    )
    .unwrap()
}

fn goal_config(enabled: bool, verifier: &str, limits: ConfigGoalBudgetLimits) -> Config {
    let mut config = Config {
        goal: GoalConfig {
            enabled,
            // A normalized `None` means explicitly unlimited in these legacy
            // fixtures, not an unconfigured default.
            default_token_limit: Some(limits.token_limit.unwrap_or(0)),
            default_cost_limit_usd: Some(limits.cost_limit_usd.unwrap_or(0.0)),
            include_channel_turn_context: true,
            verifier: zeroclaw_config::goal::GoalVerifierConfig {
                model_provider: ModelProviderRef::new(verifier),
                model: None,
            },
        },
        ..Default::default()
    };
    config.providers.models.openai.insert(
        "default".to_owned(),
        OpenAIModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                model: Some("gpt-test".to_owned()),
                ..Default::default()
            },
        },
    );
    config
}

#[test]
fn goal_config_fixture_resolves_the_configured_openai_model() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    assert!(config.validate_goal_config().is_ok());
}

#[test]
fn unset_enabled_defaults_do_not_prove_unlimited_budget() {
    let mut config = valid_goal_config(true);
    config.goal.default_token_limit = None;
    config.goal.default_cost_limit_usd = None;
    assert!(matches!(
        config.goal.default_budget().unwrap(),
        zeroclaw_config::goal::GoalDefaultBudget::Unconfigured
    ));
}

#[test]
fn explicitly_unlimited_enabled_config_retains_its_declaration_provenance() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(0),
            cost_limit_usd: Some(0.0),
        },
    );
    assert!(GoalHostSettings::from_config(&config, 42, "test-boot").is_ok());
}

fn valid_goal_config(enabled: bool) -> Config {
    goal_config(
        enabled,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    )
}

fn host_settings(config: &Config) -> GoalHostSettings<'_> {
    GoalHostSettings::from_config(config, 42, "test-boot").unwrap()
}

fn assert_admission_error(error: anyhow::Error, code: ConfigApiCode, path: &str) {
    assert_eq!(
        error.to_string(),
        "Goal configuration does not admit execution"
    );
    let structured = ConfigApiError::from_validation(error);
    assert_eq!(structured.code, code);
    assert_eq!(structured.path.as_deref(), Some(path));
}

#[tokio::test]
async fn host_rejects_missing_verifier_before_binding_start_or_resume() {
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();

    for verifier in ["", "   "] {
        let config = goal_config(
            true,
            verifier,
            ConfigGoalBudgetLimits {
                token_limit: Some(100),
                cost_limit_usd: Some(1.0),
            },
        );
        for command in [
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
            GoalCommand::Resume { response: None },
        ] {
            let error = host
                .submit(
                    &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                    ingress.clone(),
                    driver.clone(),
                    command,
                )
                .await
                .unwrap_err();
            assert_admission_error(
                error,
                ConfigApiCode::RequiredFieldEmpty,
                "goal.verifier.model_provider",
            );
        }
    }

    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn host_rejects_malformed_and_dangling_verifiers_before_binding() {
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();
    for (verifier, code) in [
        ("openai.missing", ConfigApiCode::DanglingReference),
        ("unknownfamily.default", ConfigApiCode::DanglingReference),
        ("openai", ConfigApiCode::InvalidFormat),
        (".default", ConfigApiCode::InvalidFormat),
        ("openai.", ConfigApiCode::InvalidFormat),
    ] {
        let config = goal_config(
            true,
            verifier,
            ConfigGoalBudgetLimits {
                token_limit: Some(100),
                cost_limit_usd: Some(1.0),
            },
        );
        let error = host
            .submit(
                &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "finish the task".into(),
                },
            )
            .await
            .unwrap_err();
        assert_admission_error(error, code, "goal.verifier.model_provider");
    }
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn controller_rechecks_invalid_config_without_replacing_terminal_predecessor() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();
    let valid = valid_goal_config(true);

    let start = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "preserve the terminal predecessor".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&valid), &start)
        .await
        .unwrap()
    else {
        panic!("expected initial Goal to start");
    };
    let cancel = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&valid), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));
    let predecessor = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .expect("cancelled predecessor remains current");
    assert_eq!(predecessor.id, started.task_id);
    assert_eq!(predecessor.status, TaskStatus::Cancelled);

    let attempted_start = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "must not replace predecessor".into(),
            },
        )
        .await
        .unwrap();
    let invalid_config = goal_config(
        true,
        "openai.missing",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let invalid = GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap();
    assert_admission_error(
        controller
            .submit(&invalid, &attempted_start)
            .await
            .unwrap_err(),
        ConfigApiCode::DanglingReference,
        "goal.verifier.model_provider",
    );
    let unchanged = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .expect("failed controller admission must retain predecessor");
    assert_eq!(unchanged.id, predecessor.id);
    assert_eq!(unchanged.status, TaskStatus::Cancelled);
    assert_eq!(unchanged.execution_epoch, predecessor.execution_epoch);
}

#[tokio::test]
async fn acquisition_rechecks_current_config_and_restored_config_can_acquire() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: Arc::new(AtomicUsize::new(0)),
    });
    let host = GoalExecutionHost::new();
    let valid = valid_goal_config(true);
    let start = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "acquire only while the verifier remains configured".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&valid), &start)
        .await
        .unwrap()
    else {
        panic!("expected Goal to start under valid config");
    };
    let scope = GoalExecutionScope::new(
        started.task_id.as_str(),
        ingress.session_key().durable_id(),
        started.execution_epoch,
    )
    .unwrap();
    let invalid_config = goal_config(
        true,
        "openai.missing",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let invalid = GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap();
    assert_admission_error(
        match host
            .acquire_execution(&invalid, start, &scope, &GoalParentTurnCheckpoint::new())
            .await
        {
            Ok(_) => panic!("invalid current config must not acquire execution"),
            Err(error) => error,
        },
        ConfigApiCode::DanglingReference,
        "goal.verifier.model_provider",
    );
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);

    let restored = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let fresh_submission = host
        .submit(
            &host_settings(&restored),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    drop(
        host.acquire_execution(
            &host_settings(&restored),
            fresh_submission,
            &scope,
            &GoalParentTurnCheckpoint::new(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_verifier_preserves_admin_controls_and_rejected_resume_epoch() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();
    let valid = valid_goal_config(true);
    let start = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "remain paused until valid config returns".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(_) = controller
        .submit(&host_settings(&valid), &start)
        .await
        .unwrap()
    else {
        panic!("expected Goal start");
    };
    let pause = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();
    let GoalResponse::Paused(paused) = controller
        .submit(&host_settings(&valid), &pause)
        .await
        .unwrap()
    else {
        panic!("expected Goal pause");
    };

    let invalid_config = goal_config(
        true,
        "",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let resume = host
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    assert_admission_error(
        controller
            .submit(
                &GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap(),
                &resume,
            )
            .await
            .unwrap_err(),
        ConfigApiCode::RequiredFieldEmpty,
        "goal.verifier.model_provider",
    );
    let after_rejection = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .expect("rejected resume leaves the paused task current");
    assert_eq!(after_rejection.status, TaskStatus::Paused);
    assert_eq!(after_rejection.execution_epoch, paused.execution_epoch);

    let invalid_limits = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(i64::MAX as u64 + 1),
            cost_limit_usd: Some(1.0),
        },
    );
    let status = host
        .submit(
            &GoalHostSettings::from_config(&invalid_limits, 42, "test-boot").unwrap(),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    let GoalResponse::Status(status) = controller
        .submit(
            &GoalHostSettings::from_config(&invalid_limits, 42, "test-boot").unwrap(),
            &status,
        )
        .await
        .unwrap()
    else {
        panic!("invalid defaults must not block Status");
    };
    assert_eq!(status.status, TaskStatus::Paused);
    assert_eq!(status.execution_epoch, paused.execution_epoch);

    let commands = [
        (GoalCommand::Status, 0),
        (GoalCommand::Budget, 1),
        (
            GoalCommand::SetBudget(zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                zeroclaw_commands::goal::GoalBudgetLimits {
                    token_limit: Some(12),
                    cost_limit_usd: None,
                },
            )),
            2,
        ),
        (GoalCommand::Pause { immediate: false }, 3),
        (GoalCommand::Cancel, 4),
        (GoalCommand::Help, 5),
    ];
    for (command, expected) in commands {
        let submission = host
            .submit(
                &GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap(),
                ingress.clone(),
                driver.clone(),
                command,
            )
            .await
            .unwrap();
        let response = controller
            .submit(
                &GoalHostSettings::from_config(&invalid_config, 42, "test-boot").unwrap(),
                &submission,
            )
            .await
            .unwrap();
        assert!(
            matches!(
                (&response, expected),
                (GoalResponse::Status(_), 0)
                    | (GoalResponse::Budget(_), 1)
                    | (GoalResponse::BudgetUpdated(_), 2)
                    | (GoalResponse::AlreadyPaused(_), 3)
                    | (GoalResponse::Cancelled(_), 4)
                    | (GoalResponse::Help, 5)
            ),
            "unexpected response for command {expected}: {response:?}"
        );
    }
    assert_eq!(driver.binds.load(Ordering::SeqCst), 9);
}

#[tokio::test]
async fn disabled_config_keeps_invalid_verifier_nonblocking() {
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();
    for verifier in ["openai.missing", ""] {
        let config = goal_config(
            false,
            verifier,
            ConfigGoalBudgetLimits {
                token_limit: Some(100),
                cost_limit_usd: Some(1.0),
            },
        );
        assert!(config.validate().is_ok());
        let submission = host
            .submit(
                &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Status,
            )
            .await
            .unwrap();
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
        assert!(matches!(
            controller
                .submit(
                    &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                    &submission,
                )
                .await
                .unwrap(),
            GoalResponse::Disabled
        ));
        let scope =
            GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
        let error = match host
            .acquire_execution(
                &GoalHostSettings::from_config(&config, 42, "test-boot").unwrap(),
                submission,
                &scope,
                &GoalParentTurnCheckpoint::new(),
            )
            .await
        {
            Ok(_) => panic!("disabled Goal Mode must not acquire execution"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("disabled"));
    }
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_unlimited_goal_defaults_remain_unlimited() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(0),
            cost_limit_usd: Some(0.0),
        },
    );
    let ingress = matrix_ingress();
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            recording_driver(&matrix_ingress()),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "use explicitly unlimited defaults".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &submission)
        .await
        .unwrap()
    else {
        panic!("expected Goal start with unlimited defaults");
    };
    assert_eq!(started.token_limit, None);
    assert_eq!(started.cost_limit_usd, None);
}

#[tokio::test]
async fn start_without_budget_flags_or_defaults_returns_budget_required() {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let session_id = ingress.session_key().durable_id();
        let driver = recording_driver(&ingress);
        let mut config = valid_goal_config(true);
        config.goal.default_token_limit = None;
        config.goal.default_cost_limit_usd = None;
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let submission = GoalExecutionHost::new()
            .submit(
                &host_settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "start without a configured budget".into(),
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            controller
                .submit(&host_settings(&config), &submission)
                .await
                .unwrap(),
            GoalResponse::BudgetRequired
        ));
        assert!(
            store
                .current_goal_for_session(&session_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn explicit_command_budgets_start_without_configured_defaults() {
    use zeroclaw_commands::goal::{GoalBudgetLimits, GoalBudgetSelection};

    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        for (budget, expected_tokens, expected_cost) in [
            (
                GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: Some(10),
                    cost_limit_usd: None,
                }),
                Some(10),
                None,
            ),
            (
                GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: None,
                    cost_limit_usd: Some(2.5),
                }),
                None,
                Some(2.5),
            ),
            (
                GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: Some(10),
                    cost_limit_usd: Some(2.5),
                }),
                Some(10),
                Some(2.5),
            ),
            (GoalBudgetSelection::Unlimited, None, None),
        ] {
            let mut config = valid_goal_config(true);
            config.goal.default_token_limit = None;
            config.goal.default_cost_limit_usd = None;
            let driver = recording_driver(&ingress);
            let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
            let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
            let submission = GoalExecutionHost::new()
                .submit(
                    &host_settings(&config),
                    ingress.clone(),
                    driver,
                    GoalCommand::Start {
                        budget,
                        objective: "start with the explicit command budget".into(),
                    },
                )
                .await
                .unwrap();
            let GoalResponse::Started(started) = controller
                .submit(&host_settings(&config), &submission)
                .await
                .unwrap()
            else {
                panic!("expected explicit command budget to admit Goal start");
            };
            assert_eq!(started.token_limit, expected_tokens);
            assert_eq!(started.cost_limit_usd, expected_cost);
        }
    }
}

#[tokio::test]
async fn single_positive_default_starts_with_that_limit() {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let mut config = goal_config(
            true,
            "openai.default",
            ConfigGoalBudgetLimits {
                token_limit: Some(500),
                cost_limit_usd: None,
            },
        );
        config.goal.default_cost_limit_usd = None;
        let driver = recording_driver(&ingress);
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
        let submission = GoalExecutionHost::new()
            .submit(
                &host_settings(&config),
                ingress,
                driver,
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "use the single configured token limit".into(),
                },
            )
            .await
            .unwrap();
        let GoalResponse::Started(started) = controller
            .submit(&host_settings(&config), &submission)
            .await
            .unwrap()
        else {
            panic!("expected positive token default to admit Goal start");
        };
        assert_eq!(started.token_limit, Some(500));
        assert_eq!(started.cost_limit_usd, None);
    }
}

#[tokio::test]
async fn zero_token_default_without_cost_default_returns_budget_required() {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let session_id = ingress.session_key().durable_id();
        let mut config = goal_config(
            true,
            "openai.default",
            ConfigGoalBudgetLimits {
                token_limit: Some(0),
                cost_limit_usd: None,
            },
        );
        config.goal.default_cost_limit_usd = None;
        let driver = recording_driver(&ingress);
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let submission = GoalExecutionHost::new()
            .submit(
                &host_settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "do not treat a zero-only default as a budget".into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            controller
                .submit(&host_settings(&config), &submission)
                .await
                .unwrap(),
            GoalResponse::BudgetRequired
        ));
        assert!(
            store
                .current_goal_for_session(&session_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn invalid_defaults_reject_explicit_budget_start_with_typed_path() {
    use zeroclaw_commands::goal::{GoalBudgetLimits, GoalBudgetSelection};

    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let config = goal_config(
            true,
            "openai.default",
            ConfigGoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: Some(-1.0),
            },
        );
        let driver = recording_driver(&ingress);
        let error = GoalExecutionHost::new()
            .submit(
                &host_settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Limits(GoalBudgetLimits {
                        token_limit: Some(10),
                        cost_limit_usd: None,
                    }),
                    objective: "invalid defaults must reject before binding".into(),
                },
            )
            .await
            .unwrap_err();
        assert_admission_error(
            error,
            ConfigApiCode::InvalidNumericRange,
            "goal.default_cost_limit_usd",
        );
        assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn invalid_goal_limits_are_rejected_before_binding_with_typed_path() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(i64::MAX as u64 + 1),
            cost_limit_usd: Some(1.0),
        },
    );
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let error = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "must not start with an invalid limit".into(),
            },
        )
        .await
        .unwrap_err();
    assert_admission_error(
        error,
        ConfigApiCode::InvalidNumericRange,
        "goal.default_token_limit",
    );
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
}

fn recording_driver(ingress: &GoalIngressContext) -> Arc<RecordingDriver> {
    Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    })
}

#[tokio::test]
async fn supplied_driver_must_match_the_trusted_ingress_before_binding() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(GoalSessionKey::zero_code("wrong-session").unwrap()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });

    let error = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("does not match"));
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn execution_scope_mismatch_is_rejected_before_driver_acquisition() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });
    let scope = zeroclaw_goal::GoalExecutionScope::new("goal-1", "rpc_wrong-session", 1).unwrap();
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    let checkpoint = GoalParentTurnCheckpoint::new();
    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
    {
        Ok(_) => panic!("mismatched scope must not acquire an execution lease"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("does not match"));
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mismatched_driver_key_is_rejected_before_binding() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let other_key = GoalSessionKey::matrix("matrix_other_room").unwrap();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(other_key),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });
    let error = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("trusted ingress"));
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn driver_returning_a_different_binding_is_rejected_after_binding() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let returned_key = GoalSessionKey::matrix("matrix_other_room").unwrap();
    let driver = Arc::new(WrongBindingDriver {
        advertised: ingress.session_key().clone(),
        returned_binding: GoalSessionBinding::new(returned_key),
        binds: AtomicUsize::new(0),
    });

    let error = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("binding does not match"));
    assert_eq!(driver.binds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn matching_execution_scope_returns_a_working_session_lease() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let delivered = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: delivered.clone(),
    });
    let scope = GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
    let operation = GoalOperationScope::new(scope.clone());
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    let checkpoint = GoalParentTurnCheckpoint::new();
    let mut lease = host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
        .unwrap();

    assert!(lease.canonical_history().unwrap().is_empty());
    assert_eq!(
        lease
            .run_parent_turn(
                &operation,
                GoalParentTurn {
                    kind: GoalParentTurnKind::Start,
                    objective: "finish the task".into(),
                    working_history: Vec::new(),
                }
            )
            .await
            .unwrap()
            .candidate(),
        "parent:finish the task"
    );
    assert_eq!(
        lease
            .run_verifier(
                &operation,
                GoalVerifierTurn {
                    objective: "finish the task".into(),
                    candidate: "candidate".into(),
                }
            )
            .await
            .unwrap(),
        GoalVerifierResult {
            response: "verifier:candidate".to_owned(),
            attribution: None,
        }
    );
    lease
        .append_parent_candidate("candidate".into())
        .await
        .unwrap();

    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 1);
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScriptedStep {
    Parent(Vec<(String, String)>),
    Append(String, TaskStatus, i64),
    Verifier(String),
    Resume(String),
}

#[derive(Clone, Copy)]
enum ScriptedFault {
    FailAppend,
    PauseDuringAppend,
    PauseDuringVerifier,
    ParentError,
    ParentPanic,
    PauseThenParentError,
    VerifierError,
    PauseThenVerifierError,
    ParentAccountingFailure,
    VerifierAccountingFailure,
    SettleIncomplete,
    ParentSpendRefusal(SpendRefusalCase),
    VerifierSpendRefusal(SpendRefusalCase),
}

#[derive(Clone, Copy, Debug)]
enum SpendRefusalCase {
    Clean,
    Raw,
    SameText,
    NestedNegative,
    Superseded,
    TypedAccounting,
    Durable(GoalAccountingState),
    Pending,
    OperatorPause,
    Cancel,
    BadAccountingRead,
}

#[derive(Debug)]
struct ForeignSpendSource(anyhow::Error);

impl std::fmt::Display for ForeignSpendSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("fixture foreign spend source")
    }
}

impl std::error::Error for ForeignSpendSource {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

struct ExecutionScript {
    parents: VecDeque<String>,
    verifiers: VecDeque<String>,
    steps: Vec<ScriptedStep>,
    fault: Option<ScriptedFault>,
    published_core_errors: Vec<String>,
    published_notices: Vec<zeroclaw_goal::GoalExecutionNotice>,
}

struct ScriptedDriver {
    binding: GoalSessionBinding,
    store: Arc<SqliteTaskStore>,
    sessions: Arc<SqliteSessionBackend>,
    script: Arc<StdMutex<ExecutionScript>>,
    database_path: PathBuf,
}

struct ScriptedLease {
    session_key: GoalSessionKey,
    store: Arc<SqliteTaskStore>,
    sessions: Arc<SqliteSessionBackend>,
    script: Arc<StdMutex<ExecutionScript>>,
    scope: Option<GoalExecutionScope>,
    database_path: PathBuf,
}

impl ScriptedLease {
    async fn spend_refusal(
        &self,
        scope: &GoalExecutionScope,
        case: SpendRefusalCase,
    ) -> anyhow::Error {
        use zeroclaw_api::turn_stop::{TurnStop, TurnStopCode, tag};
        use zeroclaw_goal::accounting::GoalOrdinarySpendRefusal;

        if matches!(
            case,
            SpendRefusalCase::Durable(_) | SpendRefusalCase::Pending
        ) {
            // Record uncertainty at the real registry owner after execution
            // begins, so the engine's initial admission is not the tested guard.
            assert_eq!(
                self.store
                    .admit_pending_operation(
                        scope.task_id(),
                        scope.session_id(),
                        scope.execution_epoch(),
                        "refused-prior-call",
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
            if let SpendRefusalCase::Durable(state) = case {
                assert_eq!(
                    self.store
                        .settle_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "refused-prior-call",
                            state,
                        )
                        .await
                        .unwrap(),
                    GoalTransitionResult::Applied
                );
            }
        }
        match case {
            SpendRefusalCase::OperatorPause => self.pause(scope).await,
            SpendRefusalCase::Cancel => {
                assert_eq!(
                    self.store
                        .finish_session_goal(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            TaskStatus::Cancelled,
                            Some("operator_cancel".into()),
                        )
                        .await
                        .unwrap(),
                    GoalTransitionResult::Applied
                );
            }
            SpendRefusalCase::BadAccountingRead => {
                let connection = rusqlite::Connection::open(&self.database_path).unwrap();
                connection
                    .execute(
                        "UPDATE goal_tasks SET blockers_json = 'invalid-json' WHERE task_id = ?1",
                        [scope.task_id()],
                    )
                    .unwrap();
            }
            _ => {}
        }
        let native = || {
            anyhow::Error::new(TurnStop::fatal(
                TurnStopCode::BudgetExhausted,
                "fixture ordinary daily spend refused",
            ))
        };
        if matches!(case, SpendRefusalCase::Raw) {
            return native();
        }
        if matches!(case, SpendRefusalCase::SameText) {
            return anyhow::Error::msg("fixture ordinary daily spend refused");
        }
        let inner = if matches!(case, SpendRefusalCase::NestedNegative) {
            anyhow::Error::new(GoalOrdinarySpendRefusal::new(native(), false))
        } else {
            native()
        };
        let checked = anyhow::Error::new(GoalOrdinarySpendRefusal::new(
            anyhow::Error::new(ForeignSpendSource(inner)),
            true,
        ));
        match case {
            SpendRefusalCase::Superseded => tag(
                checked,
                TurnStop::close_out(TurnStopCode::StepTimeout, "fixture decisive timeout"),
            ),
            SpendRefusalCase::TypedAccounting => GoalAccountingFailure::with_operation_source(
                GoalAccountingState::Invalid,
                false,
                checked,
            )
            .into(),
            _ => checked,
        }
    }

    async fn pause(&self, scope: &GoalExecutionScope) {
        assert_eq!(
            self.store
                .pause_session_goal(
                    scope.task_id(),
                    scope.session_id(),
                    scope.execution_epoch(),
                    GoalPauseState {
                        reason: GoalPauseReason::OperatorPaused,
                        description: None,
                        blockers: Vec::new(),
                    },
                )
                .await
                .unwrap(),
            GoalTransitionResult::Applied
        );
    }

    fn accounting_error() -> anyhow::Error {
        anyhow::Error::new(GoalAccountingFailure::settled(
            GoalAccountingState::Invalid,
            &["fixture pricing failed".to_owned()],
        ))
        .context("fixture turn context")
    }
}

#[async_trait]
impl GoalSessionDriver for ScriptedDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.binding.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(self.binding.clone(), ()))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        Ok(Box::new(ScriptedLease {
            session_key: self.binding.session_key().clone(),
            store: Arc::clone(&self.store),
            sessions: Arc::clone(&self.sessions),
            script: Arc::clone(&self.script),
            scope: None,
            database_path: self.database_path.clone(),
        }))
    }
}

#[async_trait]
impl GoalSessionExecutionLease for ScriptedLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<ChatMessage>> {
        Ok(self.sessions.try_load(&self.session_key.durable_id())?)
    }

    async fn append_resume_response(&mut self, response: String) -> anyhow::Result<()> {
        self.sessions.append(
            &self.session_key.durable_id(),
            &ChatMessage::user(&response),
        )?;
        self.script
            .lock()
            .unwrap()
            .steps
            .push(ScriptedStep::Resume(response));
        Ok(())
    }

    async fn run_parent_turn(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        let scope = operation.execution();
        if self.scope.is_none() {
            self.scope = Some(scope.clone());
        }
        let fault = {
            let mut script = self.script.lock().unwrap();
            script.steps.push(ScriptedStep::Parent(
                turn.working_history
                    .iter()
                    .map(|message| (message.role.clone(), message.content.clone()))
                    .collect(),
            ));
            script.fault
        };
        match fault {
            Some(ScriptedFault::ParentSpendRefusal(case)) => {
                return Err(self.spend_refusal(scope, case).await);
            }
            Some(ScriptedFault::ParentError) => anyhow::bail!("fixture provider failed"),
            Some(ScriptedFault::ParentPanic) => panic!("fixture parent panicked"),
            Some(ScriptedFault::PauseThenParentError) => {
                self.pause(scope).await;
                anyhow::bail!("fixture provider failed");
            }
            Some(ScriptedFault::ParentAccountingFailure) => {
                return Err(Self::accounting_error());
            }
            Some(ScriptedFault::SettleIncomplete) => {
                assert_eq!(
                    self.store
                        .admit_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "fixture-operation",
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
                assert_eq!(
                    self.store
                        .settle_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "fixture-operation",
                            GoalAccountingState::Invalid,
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
            _ => {}
        }
        let candidate = self.script.lock().unwrap().parents.pop_front().unwrap();
        let mut completed_history = turn.working_history;
        completed_history.push(ChatMessage::assistant(candidate.clone()));
        GoalParentTurnResult::from_completed_agent_turn(candidate, completed_history)
    }

    async fn run_verifier(
        &mut self,
        operation: &GoalOperationScope,
        turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        let scope = operation.execution();
        let fault = {
            let mut script = self.script.lock().unwrap();
            script.steps.push(ScriptedStep::Verifier(turn.candidate));
            script.fault
        };
        match fault {
            Some(ScriptedFault::VerifierSpendRefusal(case)) => {
                return Err(self.spend_refusal(scope, case).await);
            }
            Some(ScriptedFault::PauseDuringVerifier) => self.pause(scope).await,
            Some(ScriptedFault::VerifierError) => anyhow::bail!("fixture verifier failed"),
            Some(ScriptedFault::PauseThenVerifierError) => {
                self.pause(scope).await;
                anyhow::bail!("fixture verifier failed");
            }
            Some(ScriptedFault::VerifierAccountingFailure) => {
                return Err(Self::accounting_error());
            }
            _ => {}
        }
        Ok(GoalVerifierResult {
            response: self.script.lock().unwrap().verifiers.pop_front().unwrap(),
            attribution: None,
        })
    }

    async fn publish_goal_notice(
        &mut self,
        notice: zeroclaw_goal::GoalExecutionNotice,
    ) -> anyhow::Result<()> {
        self.script.lock().unwrap().published_notices.push(notice);
        Ok(())
    }

    async fn publish_goal_core_error(&mut self, error: &anyhow::Error) -> anyhow::Result<()> {
        if matches!(
            self.script.lock().unwrap().fault,
            Some(
                ScriptedFault::ParentSpendRefusal(SpendRefusalCase::Clean)
                    | ScriptedFault::VerifierSpendRefusal(SpendRefusalCase::Clean)
            )
        ) {
            assert_eq!(
                zeroclaw_api::turn_stop::turn_stop(error).unwrap().code,
                zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted
            );
        }
        self.script
            .lock()
            .unwrap()
            .published_core_errors
            .push(format!("{error:#}"));
        Ok(())
    }

    async fn append_parent_candidate(&mut self, candidate: String) -> anyhow::Result<()> {
        let fault = self.script.lock().unwrap().fault;
        if matches!(fault, Some(ScriptedFault::FailAppend)) {
            anyhow::bail!("fixture append failed");
        }
        let scope = self.scope.as_ref().unwrap();
        let task = self
            .store
            .current_goal_for_session(scope.session_id())
            .await?
            .unwrap();
        self.sessions
            .append(scope.session_id(), &ChatMessage::assistant(&candidate))?;
        self.script.lock().unwrap().steps.push(ScriptedStep::Append(
            candidate,
            task.status,
            task.execution_epoch,
        ));
        if matches!(fault, Some(ScriptedFault::PauseDuringAppend)) {
            self.pause(scope).await;
        }
        Ok(())
    }
}

struct ScriptedAccounting;

#[async_trait]
impl GoalOperationAccounting for ScriptedAccounting {
    async fn admit(&self, _request: GoalOperationRequest) -> anyhow::Result<()> {
        Ok(())
    }

    async fn settle(&self, _settlement: GoalOperationSettlement) -> anyhow::Result<()> {
        Ok(())
    }

    async fn pricing_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct ScriptedAccountingFactory;

impl GoalOperationAccountingFactory for ScriptedAccountingFactory {
    fn for_scope(&self, _scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting> {
        Arc::new(ScriptedAccounting)
    }
}

struct EngineFixture {
    config: Config,
    runtime: GoalRuntime,
    store: Arc<SqliteTaskStore>,
    driver: Arc<ScriptedDriver>,
    scope: GoalExecutionScope,
    request: Option<zeroclaw_goal::GoalExecutionRequest>,
    session_data: PathBuf,
    // Drop both databases and their shared handles before the temporary files.
    _directory: tempfile::TempDir,
}

impl EngineFixture {
    async fn new(parents: &[&str], verifiers: &[&str], fault: Option<ScriptedFault>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteTaskStore::new(&directory.path().join("control")).unwrap());
        let session_data = directory.path().join("history");
        let config = valid_goal_config(true);
        let ingress = matrix_ingress();
        let driver = Arc::new(ScriptedDriver {
            binding: GoalSessionBinding::new(ingress.session_key().clone()),
            store: Arc::clone(&store),
            database_path: directory.path().join("control/control_plane.db"),
            sessions: Arc::new(SqliteSessionBackend::new(&session_data).unwrap()),
            script: Arc::new(StdMutex::new(ExecutionScript {
                parents: parents.iter().map(|text| (*text).to_owned()).collect(),
                verifiers: verifiers.iter().map(|text| (*text).to_owned()).collect(),
                steps: Vec::new(),
                fault,
                published_core_errors: Vec::new(),
                published_notices: Vec::new(),
            })),
        });
        let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let (_, request) = runtime
            .submit(
                &host_settings(&config),
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                        zeroclaw_commands::goal::GoalBudgetLimits {
                            token_limit: Some(100),
                            cost_limit_usd: None,
                        },
                    ),
                    objective: "finish the bounded task".into(),
                },
            )
            .await
            .unwrap()
            .into_parts();
        let request = request.unwrap();
        let scope = request.scope().clone();
        Self {
            config,
            runtime,
            store,
            driver,
            scope,
            request: Some(request),
            session_data,
            _directory: directory,
        }
    }

    async fn run(&mut self) -> anyhow::Result<zeroclaw_goal::GoalExecutionOutcome> {
        self.run_with_accounting(Arc::new(ScriptedAccountingFactory))
            .await
    }

    async fn run_with_accounting(
        &mut self,
        accounting: Arc<dyn GoalOperationAccountingFactory>,
    ) -> anyhow::Result<zeroclaw_goal::GoalExecutionOutcome> {
        // The engine resolves settings from a live handle at acquisition.
        let live = LiveConfig::new(self.config.clone());
        let source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot")?;
        self.runtime
            .execution_engine(accounting)
            .run(
                &source,
                self.request.take().unwrap(),
                GoalParentTurnCheckpoint::new(),
            )
            .await
    }

    async fn task(&self) -> TaskRecord {
        self.store
            .current_goal_for_session(self.scope.session_id())
            .await
            .unwrap()
            .unwrap()
    }

    async fn reason(&self) -> Option<String> {
        self.store
            .terminal_reason_for_session_goal(self.scope.task_id(), self.scope.session_id())
            .await
            .unwrap()
    }

    fn steps(&self) -> Vec<ScriptedStep> {
        self.driver.script.lock().unwrap().steps.clone()
    }

    fn history(&self) -> Vec<(String, String)> {
        // A separate backend handle proves the transcript is not only a cache.
        SqliteSessionBackend::new(&self.session_data)
            .unwrap()
            .try_load(self.scope.session_id())
            .unwrap()
            .into_iter()
            .map(|message| (message.role, message.content))
            .collect()
    }
}

mod supervisor_error_contract {
    use super::*;
    use tokio::sync::Semaphore;

    #[derive(Clone, Copy)]
    enum HeldTurn {
        Parent,
        Verifier,
    }

    struct HeldDriver {
        inner: Arc<ScriptedDriver>,
        turn: HeldTurn,
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
        accounting: Option<GoalAccountingState>,
        command: Arc<tokio::sync::Mutex<()>>,
        binds: AtomicUsize,
        hold_rebind: AtomicBool,
        rebind_entered: Semaphore,
        release_rebind: Semaphore,
    }

    struct HeldLease {
        inner: Box<dyn GoalSessionExecutionLease>,
        store: Arc<SqliteTaskStore>,
        turn: HeldTurn,
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
        accounting: Option<GoalAccountingState>,
    }

    #[async_trait]
    impl GoalSessionDriver for HeldDriver {
        fn session_key(&self) -> &GoalSessionKey {
            self.inner.session_key()
        }

        async fn bind(&self, _ingress: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
            let ordinal = self.binds.fetch_add(1, Ordering::SeqCst) + 1;
            let command = self.command.clone().lock_owned().await;
            if ordinal == 2 && self.hold_rebind.load(Ordering::SeqCst) {
                self.rebind_entered.add_permits(1);
                self.release_rebind.acquire().await.unwrap().forget();
            }
            Ok(GoalSessionLease::new(self.inner.binding.clone(), command))
        }

        async fn acquire_execution(
            &self,
            ingress: &GoalIngressContext,
            scope: &GoalExecutionScope,
            checkpoint: &GoalParentTurnCheckpoint,
        ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
            Ok(Box::new(HeldLease {
                inner: self
                    .inner
                    .acquire_execution(ingress, scope, checkpoint)
                    .await?,
                store: self.inner.store.clone(),
                turn: self.turn,
                entered: self.entered.clone(),
                release: self.release.clone(),
                accounting: self.accounting,
            }))
        }
    }

    #[async_trait]
    impl GoalSessionExecutionLease for HeldLease {
        fn session_key(&self) -> &GoalSessionKey {
            self.inner.session_key()
        }

        fn canonical_history(&self) -> anyhow::Result<Vec<ChatMessage>> {
            self.inner.canonical_history()
        }

        async fn append_resume_response(&mut self, response: String) -> anyhow::Result<()> {
            self.inner.append_resume_response(response).await
        }

        async fn append_parent_candidate(&mut self, candidate: String) -> anyhow::Result<()> {
            self.inner.append_parent_candidate(candidate).await
        }

        async fn run_parent_turn(
            &mut self,
            operation: &GoalOperationScope,
            turn: GoalParentTurn,
        ) -> anyhow::Result<GoalParentTurnResult> {
            let scope = operation.execution();
            if self.accounting.is_some() {
                assert_eq!(
                    self.store
                        .admit_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "held-operation",
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
            if matches!(self.turn, HeldTurn::Parent) {
                self.entered.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            if let Some(state) = self.accounting
                && state != GoalAccountingState::OutcomeUnknown
            {
                assert_eq!(
                    self.store
                        .settle_pending_operation(
                            scope.task_id(),
                            scope.session_id(),
                            scope.execution_epoch(),
                            "held-operation",
                            state,
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
            self.inner.run_parent_turn(operation, turn).await
        }

        async fn run_verifier(
            &mut self,
            operation: &GoalOperationScope,
            turn: GoalVerifierTurn,
        ) -> anyhow::Result<GoalVerifierResult> {
            if matches!(self.turn, HeldTurn::Verifier) {
                self.entered.add_permits(1);
                self.release.acquire().await.unwrap().forget();
            }
            self.inner.run_verifier(operation, turn).await
        }

        async fn publish_goal_notice(
            &mut self,
            notice: zeroclaw_goal::GoalExecutionNotice,
        ) -> anyhow::Result<()> {
            self.inner.publish_goal_notice(notice).await
        }

        async fn publish_goal_core_error(&mut self, error: &anyhow::Error) -> anyhow::Result<()> {
            self.inner.publish_goal_core_error(error).await
        }
    }

    struct Fixture {
        config: Config,
        source: GoalHostSettingsSource,
        store: Arc<SqliteTaskStore>,
        driver: Arc<HeldDriver>,
        supervisor: Arc<GoalExecutionSupervisor>,
        scope: GoalExecutionScope,
        started: Option<zeroclaw_goal::GoalExecutionSubmission>,
        _directory: TempDir,
    }

    impl Fixture {
        async fn new(
            turn: HeldTurn,
            fault: ScriptedFault,
            accounting: Option<GoalAccountingState>,
        ) -> Self {
            let directory = TempDir::new().unwrap();
            let store = Arc::new(SqliteTaskStore::new(&directory.path().join("control")).unwrap());
            let config = valid_goal_config(true);
            let ingress = matrix_ingress();
            let inner = Arc::new(ScriptedDriver {
                binding: GoalSessionBinding::new(ingress.session_key().clone()),
                store: store.clone(),
                database_path: directory.path().join("control/control_plane.db"),
                sessions: Arc::new(
                    SqliteSessionBackend::new(&directory.path().join("history")).unwrap(),
                ),
                script: Arc::new(StdMutex::new(ExecutionScript {
                    parents: VecDeque::from(["candidate".to_owned()]),
                    verifiers: VecDeque::from([SCRIPTED_COMPLETE.to_owned()]),
                    steps: Vec::new(),
                    fault: Some(fault),
                    published_core_errors: Vec::new(),
                    published_notices: Vec::new(),
                })),
            });
            let driver = Arc::new(HeldDriver {
                inner,
                turn,
                entered: Arc::new(Semaphore::new(0)),
                release: Arc::new(Semaphore::new(0)),
                accounting,
                command: Arc::new(tokio::sync::Mutex::new(())),
                binds: AtomicUsize::new(0),
                hold_rebind: AtomicBool::new(false),
                rebind_entered: Semaphore::new(0),
                release_rebind: Semaphore::new(0),
            });
            let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
            let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
                runtime.execution_engine(Arc::new(ScriptedAccountingFactory)),
            )));
            let live = LiveConfig::new(config.clone());
            let source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
            let started = supervisor
                .submit(
                    &host_settings(&config),
                    ingress,
                    driver.clone(),
                    GoalCommand::Start {
                        budget: zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                            zeroclaw_commands::goal::GoalBudgetLimits {
                                token_limit: Some(100),
                                cost_limit_usd: None,
                            },
                        ),
                        objective: "finish the bounded task".into(),
                    },
                )
                .await
                .unwrap();
            let task = store
                .current_goal_for_session(&driver.session_key().durable_id())
                .await
                .unwrap()
                .unwrap();
            let scope =
                GoalExecutionScope::new(task.id, task.session_id.unwrap(), task.execution_epoch)
                    .unwrap();
            Self {
                config,
                source,
                store,
                driver,
                supervisor,
                scope,
                started: Some(started),
                _directory: directory,
            }
        }

        async fn start(&mut self) {
            self.supervisor
                .launch_after_response(&self.source, &mut self.started.take().unwrap())
                .await
                .unwrap();
            timeout(Duration::from_secs(1), self.driver.entered.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
        }

        async fn task(&self) -> TaskRecord {
            self.store
                .current_goal_for_session(self.scope.session_id())
                .await
                .unwrap()
                .unwrap()
        }

        async fn pause(&self) -> zeroclaw_goal::GoalExecutionSubmission {
            let pause = self
                .supervisor
                .submit(
                    &host_settings(&self.config),
                    matrix_ingress(),
                    self.driver.clone(),
                    GoalCommand::Pause { immediate: true },
                )
                .await
                .unwrap();
            assert!(matches!(
                pause.response(),
                GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Pause)
            ));
            pause
        }

        async fn reason(&self) -> Option<String> {
            self.store
                .terminal_reason_for_session_goal(self.scope.task_id(), self.scope.session_id())
                .await
                .unwrap()
        }

        async fn goal(&self) -> GoalTaskRecord {
            self.store
                .get_goal_task(self.scope.task_id())
                .await
                .unwrap()
                .unwrap()
        }

        fn intent_states(&self) -> Vec<(String, Option<String>)> {
            let connection = rusqlite::Connection::open_with_flags(
                self._directory.path().join("control/control_plane.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let mut statement = connection
                .prepare(
                    "SELECT state, outcome_reason FROM goal_control_intents
                 WHERE task_id = ?1 AND session_id = ?2 ORDER BY intent_id",
                )
                .unwrap();
            statement
                .query_map(
                    rusqlite::params![self.scope.task_id(), self.scope.session_id()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        }
    }

    #[tokio::test]
    async fn supervisor_checked_spend_refusal_pauses_or_hands_combined_sql_failure_once() {
        for sql_failure in [false, true] {
            let mut fixture = Fixture::new(
                HeldTurn::Parent,
                ScriptedFault::ParentSpendRefusal(SpendRefusalCase::Clean),
                None,
            )
            .await;
            if sql_failure {
                let connection = rusqlite::Connection::open(
                    fixture._directory.path().join("control/control_plane.db"),
                )
                .unwrap();
                connection.execute_batch("CREATE TRIGGER fail_spend_pause BEFORE UPDATE OF status ON tasks WHEN NEW.status = 'paused' BEGIN SELECT RAISE(FAIL, 'fixture supervised spend pause SQL failure'); END;").unwrap();
            }
            fixture.start().await;
            fixture.driver.release.add_permits(1);
            let result = fixture.supervisor.drain(&fixture.scope).await;
            if sql_failure {
                let text = result.unwrap_err().to_string();
                assert!(
                    text.contains("fixture supervised spend pause SQL failure"),
                    "{text}"
                );
                assert!(
                    text.contains("fixture ordinary daily spend refused"),
                    "{text}"
                );
                assert_eq!(fixture.task().await.status, TaskStatus::Failed);
                assert_eq!(fixture.reason().await.as_deref(), Some("executor_failed"));
            } else {
                assert_eq!(result.unwrap(), GoalExecutionOutcome::Fenced);
                assert_eq!(fixture.task().await.status, TaskStatus::Paused);
                assert_eq!(
                    fixture.goal().await.pause_reason,
                    Some(GoalPauseReason::BudgetExhausted)
                );
            }
            let script = fixture.driver.inner.script.lock().unwrap();
            assert_eq!(script.published_core_errors.len(), usize::from(sql_failure));
            assert_eq!(
                script
                    .published_notices
                    .iter()
                    .filter(|notice| matches!(
                        notice,
                        zeroclaw_goal::GoalExecutionNotice::Failed(_)
                    ))
                    .count(),
                usize::from(sql_failure)
            );
            if sql_failure {
                assert_eq!(
                    script.published_core_errors[0]
                        .matches("fixture ordinary daily spend refused")
                        .count(),
                    1
                );
                assert!(
                    script.published_core_errors[0]
                        .contains("fixture supervised spend pause SQL failure")
                );
            }
        }
    }

    #[tokio::test]
    async fn supervisor_settled_parent_and_verifier_errors_preserve_operator_pause_and_original_error()
     {
        for (turn, fault, cause) in [
            (
                HeldTurn::Parent,
                ScriptedFault::ParentError,
                "fixture provider failed",
            ),
            (
                HeldTurn::Verifier,
                ScriptedFault::VerifierError,
                "fixture verifier failed",
            ),
        ] {
            let mut fixture = Fixture::new(turn, fault, None).await;
            fixture.start().await;
            let mut pause = fixture.pause().await;
            fixture.driver.release.add_permits(1);
            let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
            assert!(format!("{error:#}").contains(cause));
            assert!(matches!(
                fixture
                    .supervisor
                    .launch_after_response(&fixture.source, &mut pause)
                    .await
                    .unwrap(),
                Some(GoalResponse::Paused(_))
            ));
            let task = fixture.task().await;
            assert_eq!(task.status, TaskStatus::Paused);
            assert_eq!(task.execution_epoch, fixture.scope.execution_epoch() + 1);
            let goal = fixture.goal().await;
            assert_eq!(goal.pause_reason, Some(GoalPauseReason::OperatorPaused));
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert_eq!(fixture.reason().await, None);
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
            let history = fixture
                .driver
                .inner
                .sessions
                .try_load(fixture.scope.session_id())
                .unwrap();
            assert_eq!(
                history.len(),
                usize::from(matches!(turn, HeldTurn::Verifier))
            );
            if matches!(turn, HeldTurn::Verifier) {
                assert_eq!(history[0].content, "candidate");
            }
            let steps = &fixture.driver.inner.script.lock().unwrap().steps;
            assert_eq!(
                steps
                    .iter()
                    .filter(|step| matches!(step, ScriptedStep::Parent(_)))
                    .count(),
                1
            );
            assert_eq!(
                steps
                    .iter()
                    .filter(|step| matches!(step, ScriptedStep::Verifier(_)))
                    .count(),
                usize::from(matches!(turn, HeldTurn::Verifier))
            );
        }
    }

    #[tokio::test]
    async fn supervisor_settled_parent_and_verifier_errors_preserve_restart_pause() {
        for (turn, fault) in [
            (HeldTurn::Parent, ScriptedFault::ParentError),
            (HeldTurn::Verifier, ScriptedFault::VerifierError),
        ] {
            let mut fixture = Fixture::new(turn, fault, None).await;
            fixture.start().await;
            let supervisor = fixture.supervisor.clone();
            let restart =
                zeroclaw_spawn::spawn!(async move { supervisor.pause_for_restart().await });
            timeout(Duration::from_secs(1), async {
                loop {
                    if fixture.task().await.status == TaskStatus::Paused {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            fixture.driver.release.add_permits(1);
            timeout(Duration::from_secs(1), restart)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let task = fixture.task().await;
            assert_eq!(task.status, TaskStatus::Paused);
            let goal = fixture.goal().await;
            assert_eq!(goal.pause_reason, Some(GoalPauseReason::DaemonRestart));
            assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            assert_eq!(fixture.reason().await, None);
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        }
    }

    #[tokio::test]
    async fn supervisor_incomplete_accounting_still_fails_after_operator_pause() {
        for state in [
            GoalAccountingState::Missing,
            GoalAccountingState::Invalid,
            GoalAccountingState::OutcomeUnknown,
        ] {
            let mut fixture =
                Fixture::new(HeldTurn::Parent, ScriptedFault::ParentError, Some(state)).await;
            fixture.start().await;
            let _pause = fixture.pause().await;
            fixture.driver.release.add_permits(1);
            let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
            assert!(format!("{error:#}").contains("fixture provider failed"));
            let task = fixture.task().await;
            assert_eq!(task.status, TaskStatus::Failed);
            let goal = fixture.goal().await;
            assert_eq!(goal.accounting_state, state);
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert_eq!(
                fixture.reason().await.as_deref(),
                Some(if state == GoalAccountingState::OutcomeUnknown {
                    "accounting_outcome_unknown"
                } else {
                    "accounting_missing_or_invalid"
                })
            );
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        }
    }

    #[tokio::test]
    async fn supervisor_unpaused_parent_error_remains_terminal() {
        let mut fixture = Fixture::new(HeldTurn::Parent, ScriptedFault::ParentError, None).await;
        fixture.start().await;
        fixture.driver.release.add_permits(1);
        let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
        assert!(format!("{error:#}").contains("fixture provider failed"));
        assert_eq!(fixture.task().await.status, TaskStatus::Failed);
        assert_eq!(
            fixture.reason().await.as_deref(),
            Some("parent_operation_failed")
        );
    }

    #[tokio::test]
    async fn supervisor_cancel_observes_worker_registered_while_waiting_for_command_lease() {
        let mut fixture = Fixture::new(HeldTurn::Parent, ScriptedFault::ParentError, None).await;
        fixture.driver.hold_rebind.store(true, Ordering::SeqCst);
        let supervisor = fixture.supervisor.clone();
        let source = fixture.source.clone();
        let mut started = fixture.started.take().unwrap();
        let launch = zeroclaw_spawn::spawn!(async move {
            supervisor
                .launch_after_response(&source, &mut started)
                .await
        });
        timeout(
            Duration::from_secs(1),
            fixture.driver.rebind_entered.acquire(),
        )
        .await
        .unwrap()
        .unwrap()
        .forget();
        let supervisor = fixture.supervisor.clone();
        let config = fixture.config.clone();
        let driver = fixture.driver.clone();
        let cancel = zeroclaw_spawn::spawn!(async move {
            supervisor
                .submit(
                    &host_settings(&config),
                    matrix_ingress(),
                    driver,
                    GoalCommand::Cancel,
                )
                .await
        });
        timeout(Duration::from_secs(1), async {
            while fixture.driver.binds.load(Ordering::SeqCst) < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fixture.driver.release_rebind.add_permits(1);
        launch.await.unwrap().unwrap();
        let mut cancel = timeout(Duration::from_secs(1), cancel)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            cancel.response(),
            GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Cancel)
        ));
        fixture.driver.release.add_permits(1);
        assert!(matches!(
            fixture
                .supervisor
                .launch_after_response(&fixture.source, &mut cancel)
                .await
                .unwrap(),
            Some(GoalResponse::Cancelled(_))
        ));
        assert_eq!(fixture.task().await.status, TaskStatus::Cancelled);
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
    }

    #[tokio::test]
    async fn supervisor_worker_panic_closes_queued_checkpoint_and_normal_drain_fails_missing_completion()
     {
        for accounting in [None, Some(GoalAccountingState::OutcomeUnknown)] {
            let mut fixture =
                Fixture::new(HeldTurn::Parent, ScriptedFault::ParentPanic, accounting).await;
            fixture.start().await;
            let zeroclaw_goal::GoalControlAdmission::Queued { deferred, .. } = fixture
                .supervisor
                .admit_control(
                    &host_settings(&fixture.config),
                    matrix_ingress(),
                    fixture.driver.clone(),
                    GoalCommand::Pause { immediate: false },
                )
                .await
                .unwrap()
            else {
                panic!("active parent must queue the control");
            };
            assert_eq!(fixture.intent_states(), vec![("pending".to_owned(), None)]);
            fixture.driver.release.add_permits(1);
            let closed = match timeout(Duration::from_secs(1), deferred.wait())
                .await
                .unwrap()
            {
                Err(closed) => closed,
                Ok(_) => panic!("a panicked worker must not admit the queued control"),
            };
            assert!(
                std::error::Error::source(&closed)
                    .unwrap()
                    .is::<zeroclaw_goal::GoalCheckpointClosed>()
            );
            let settled_intents = vec![(
                "superseded".to_owned(),
                Some("checkpoint_closed".to_owned()),
            )];
            assert_eq!(fixture.intent_states(), settled_intents);
            let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
            assert!(format!("{error:#}").contains("stopped without reporting its completion"));
            assert_eq!(fixture.task().await.status, TaskStatus::Failed);
            let goal = fixture.goal().await;
            assert_eq!(
                goal.accounting_state,
                accounting.unwrap_or(GoalAccountingState::Complete)
            );
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert_eq!(
                fixture.reason().await.as_deref(),
                Some(if accounting.is_some() {
                    "accounting_outcome_unknown"
                } else {
                    "executor_failed"
                })
            );
            assert_eq!(fixture.intent_states(), settled_intents);
            assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
        }
    }

    #[tokio::test]
    async fn supervisor_missing_completion_is_terminal_even_after_operator_pause() {
        let mut fixture = Fixture::new(HeldTurn::Parent, ScriptedFault::ParentPanic, None).await;
        fixture.start().await;
        let _pause = fixture.pause().await;
        fixture.driver.release.add_permits(1);
        let error = fixture.supervisor.drain(&fixture.scope).await.unwrap_err();
        assert!(format!("{error:#}").contains("stopped without reporting its completion"));
        assert_eq!(fixture.task().await.status, TaskStatus::Failed);
        assert_eq!(fixture.reason().await.as_deref(), Some("executor_failed"));
        assert!(!fixture.supervisor.owns_scope(&fixture.scope).await);
    }
}

const SCRIPTED_COMPLETE: &str =
    r#"{"decision":"complete","reason":"all criteria met","blockers":[]}"#;

#[tokio::test]
async fn engine_persists_a_verified_candidate_before_completing() {
    // Completion follows the single durable candidate append and verifier.
    let mut fixture = EngineFixture::new(&["final answer"], &[SCRIPTED_COMPLETE], None).await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Completed
    );
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "final answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("final answer".into()),
        ]
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Completed);
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "final answer".into())]
    );
}

#[tokio::test]
async fn engine_candidate_persistence_failure_fails_without_verifying_or_completing() {
    // An append failure ends the exact Goal without admitting its verifier.
    let mut fixture = EngineFixture::new(&["answer"], &[], Some(ScriptedFault::FailAppend)).await;
    let error = fixture.run().await.unwrap_err();
    assert!(format!("{error:#}").contains("fixture append failed"));
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("candidate_persistence_failed")
    );
    assert!(fixture.history().is_empty());
}

#[tokio::test]
async fn engine_pause_during_candidate_persistence_keeps_the_candidate_and_never_completes() {
    // A winning pause keeps the appended answer and prevents verifier admission.
    let mut fixture =
        EngineFixture::new(&["answer"], &[], Some(ScriptedFault::PauseDuringAppend)).await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Fenced
    );
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
        ]
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_pause_during_verification_keeps_the_persisted_candidate_and_never_completes() {
    // A pause committed during verification cannot be overwritten by completion.
    let mut fixture = EngineFixture::new(
        &["answer"],
        &[SCRIPTED_COMPLETE],
        Some(ScriptedFault::PauseDuringVerifier),
    )
    .await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Fenced
    );
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("answer".into()),
        ]
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_blocked_question_is_persisted_before_pause_and_retained_on_resume() {
    // Resume reads the durable question and appends its response before new work.
    let payload = serde_json::json!({"options": ["A", "B"]});
    let blocked = serde_json::json!({"decision": "blocked", "reason": "choice required", "blockers": [{"kind": "needs_user_input", "message": "Choose A or B?", "payload": payload}]}).to_string();
    let mut fixture = EngineFixture::new(
        &["Choose A or B?", "Done with B"],
        &[&blocked, SCRIPTED_COMPLETE],
        None,
    )
    .await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::VerifierBlocked
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Paused);
    let goal = fixture
        .store
        .get_goal_task(fixture.scope.task_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::NeedsUserInput));
    assert_eq!(goal.blockers[0].payload.as_ref(), Some(&payload));
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "Choose A or B?".into())]
    );
    let (_, request) = fixture
        .runtime
        .submit(
            &host_settings(&fixture.config),
            matrix_ingress(),
            fixture.driver.clone(),
            GoalCommand::Resume {
                response: Some("B".into()),
            },
        )
        .await
        .unwrap()
        .into_parts();
    let request = request.unwrap();
    fixture.scope = request.scope().clone();
    fixture.request = Some(request);
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Completed
    );
    let steps = fixture.steps();
    assert!(
        matches!(&steps[1], ScriptedStep::Append(text, TaskStatus::Running, _) if text == "Choose A or B?")
    );
    assert_eq!(steps[3], ScriptedStep::Resume("B".into()));
    assert_eq!(
        steps[4],
        ScriptedStep::Parent(vec![
            ("assistant".into(), "Choose A or B?".into()),
            ("user".into(), "B".into())
        ])
    );
    assert!(
        matches!(&steps[5], ScriptedStep::Append(text, TaskStatus::Running, _) if text == "Done with B")
    );
    assert_eq!(
        fixture.history(),
        vec![
            ("assistant".into(), "Choose A or B?".into()),
            ("user".into(), "B".into()),
            ("assistant".into(), "Done with B".into())
        ]
    );
}

#[tokio::test]
async fn engine_persists_each_continued_candidate_in_order() {
    // Continued answers persist once while feedback remains only working context.
    let feedback =
        r#"{"decision":"continue","reason":"finish the remaining criterion","blockers":[]}"#;
    let mut fixture = EngineFixture::new(
        &["draft one", "final answer"],
        &[feedback, SCRIPTED_COMPLETE],
        None,
    )
    .await;
    assert_eq!(
        fixture.run().await.unwrap(),
        zeroclaw_goal::GoalExecutionOutcome::Completed
    );
    let steps = fixture.steps();
    let appended: Vec<_> = steps
        .iter()
        .filter_map(|step| match step {
            ScriptedStep::Append(text, _, _) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(appended, vec!["draft one", "final answer"]);
    let ScriptedStep::Parent(history) = &steps[3] else {
        panic!("expected the second parent turn");
    };
    assert_eq!(
        history[history.len() - 2],
        ("assistant".into(), "draft one".into())
    );
    assert_eq!(history.last().unwrap().0, "user");
    assert!(
        history
            .last()
            .unwrap()
            .1
            .contains("finish the remaining criterion")
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Completed);
    assert_eq!(
        fixture.history(),
        vec![
            ("assistant".into(), "draft one".into()),
            ("assistant".into(), "final answer".into())
        ]
    );
}

#[tokio::test]
async fn engine_parent_accounting_failure_fails_as_accounting_with_its_diagnostic() {
    // Context layers preserve typed classification and the original diagnostic.
    let mut fixture =
        EngineFixture::new(&[], &[], Some(ScriptedFault::ParentAccountingFailure)).await;
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture pricing failed"));
    assert!(!rendered.contains("Goal parent operation failed"));
    assert!(
        error
            .chain()
            .any(|cause| cause.is::<GoalAccountingFailure>())
    );
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
}

#[tokio::test]
async fn engine_checked_spend_refusal_pauses_parent_and_verifier_without_successor() {
    for verifier in [false, true] {
        let fault = if verifier {
            ScriptedFault::VerifierSpendRefusal(SpendRefusalCase::Clean)
        } else {
            ScriptedFault::ParentSpendRefusal(SpendRefusalCase::Clean)
        };
        let mut fixture = EngineFixture::new(&["durable candidate"], &[], Some(fault)).await;
        assert_eq!(fixture.run().await.unwrap(), GoalExecutionOutcome::Fenced);
        assert_eq!(fixture.task().await.status, TaskStatus::Paused);
        let goal = fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.pause_reason, Some(GoalPauseReason::BudgetExhausted));
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
        let message = "An ordinary daily or monthly cost limit refused the next Goal call. Wait for the ordinary limit to reset or raise it, then explicitly resume.";
        assert_eq!(goal.pause_description.as_deref(), Some(message));
        assert_eq!(goal.blockers.len(), 1);
        assert_eq!(
            goal.blockers[0].kind,
            zeroclaw_goal::GoalBlockerKind::Budget
        );
        assert_eq!(goal.blockers[0].message, message);
        assert!(goal.blockers[0].payload.is_none());
        assert_eq!(fixture.steps().len(), if verifier { 3 } else { 1 });
        assert_eq!(
            fixture.history(),
            if verifier {
                vec![("assistant".into(), "durable candidate".into())]
            } else {
                Vec::new()
            }
        );
        let script = fixture.driver.script.lock().unwrap();
        assert!(script.published_core_errors.is_empty());
        assert!(script.published_notices.is_empty());
    }
}

#[tokio::test]
async fn engine_unqualified_or_superseded_spend_refusal_remains_terminal() {
    for case in [
        SpendRefusalCase::Raw,
        SpendRefusalCase::SameText,
        SpendRefusalCase::NestedNegative,
        SpendRefusalCase::Superseded,
        SpendRefusalCase::TypedAccounting,
    ] {
        for verifier in [false, true] {
            let fault = if verifier {
                ScriptedFault::VerifierSpendRefusal(case)
            } else {
                ScriptedFault::ParentSpendRefusal(case)
            };
            let mut fixture = EngineFixture::new(&["durable candidate"], &[], Some(fault)).await;
            let error = fixture.run().await.unwrap_err();
            assert!(format!("{error:#}").contains("fixture ordinary daily spend refused"));
            assert_eq!(fixture.task().await.status, TaskStatus::Failed, "{case:?}");
            let expected = if matches!(case, SpendRefusalCase::TypedAccounting) {
                "accounting_missing_or_invalid"
            } else if verifier {
                "verifier_operation_failed"
            } else {
                "parent_operation_failed"
            };
            assert_eq!(fixture.reason().await.as_deref(), Some(expected));
            assert_eq!(fixture.steps().len(), if verifier { 3 } else { 1 });
            let goal = fixture
                .store
                .get_goal_task(fixture.scope.task_id())
                .await
                .unwrap()
                .unwrap();
            assert_ne!(goal.pause_reason, Some(GoalPauseReason::BudgetExhausted));
        }
    }
}

#[tokio::test]
async fn engine_spend_refusal_preserves_durable_accounting_and_pending_precedence() {
    for case in [
        SpendRefusalCase::Durable(GoalAccountingState::Missing),
        SpendRefusalCase::Durable(GoalAccountingState::Invalid),
        SpendRefusalCase::Durable(GoalAccountingState::OutcomeUnknown),
        SpendRefusalCase::Pending,
    ] {
        for verifier in [false, true] {
            let fault = if verifier {
                ScriptedFault::VerifierSpendRefusal(case)
            } else {
                ScriptedFault::ParentSpendRefusal(case)
            };
            let mut fixture = EngineFixture::new(&["durable candidate"], &[], Some(fault)).await;
            let error = fixture.run().await.unwrap_err();
            assert_eq!(
                zeroclaw_api::turn_stop::turn_stop(&error).unwrap().code,
                zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted
            );
            let failure = error.downcast_ref::<GoalAccountingFailure>().unwrap();
            let (state, pending) = match case {
                SpendRefusalCase::Durable(state) => (state, false),
                _ => (GoalAccountingState::Complete, true),
            };
            assert_eq!(failure.accounting_state(), state);
            assert_eq!(failure.has_pending_operation(), pending);
            assert_eq!(fixture.task().await.status, TaskStatus::Failed);
            assert_eq!(
                fixture.reason().await.as_deref(),
                Some(if pending || state == GoalAccountingState::OutcomeUnknown {
                    "accounting_outcome_unknown"
                } else {
                    "accounting_missing_or_invalid"
                })
            );
            assert_eq!(fixture.steps().len(), if verifier { 3 } else { 1 });
        }
    }
}

#[tokio::test]
async fn engine_spend_refusal_requires_pricing_only_for_a_finite_goal_cost_limit() {
    for finite_cost in [false, true] {
        let mut fixture = EngineFixture::new(
            &[],
            &[],
            Some(ScriptedFault::ParentSpendRefusal(SpendRefusalCase::Clean)),
        )
        .await;
        if finite_cost {
            assert_eq!(
                fixture
                    .store
                    .update_session_goal_limits(
                        fixture.scope.task_id(),
                        fixture.scope.session_id(),
                        fixture.scope.execution_epoch(),
                        Some(100),
                        Some(1.0)
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
        }
        let costs = fixture._directory.path().join("costs");
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: true,
                    ..Default::default()
                },
                &costs,
            )
            .unwrap(),
        );
        let mut usage =
            zeroclaw_config::cost::types::TokenUsage::new("unpriced-model", 5, 2, 0, 0.0, 0.0, 0.0);
        usage.pricing_available = false;
        usage.unpriced_tokens = usage.total_tokens;
        tracker
            .record_usage_with_task_attribution(usage, Some("main"), Some(fixture.scope.task_id()))
            .unwrap();
        assert_eq!(
            tracker
                .get_strict_usage_totals_for_task_with_pricing(fixture.scope.task_id())
                .unwrap(),
            (7, 0.0, false)
        );
        let before = std::fs::read(costs.join("state/costs.jsonl")).unwrap();
        let accounting = Arc::new(
            RuntimeGoalOperationAccountingFactory::new(
                fixture.store.clone(),
                tracker.clone(),
                "main",
                Arc::new(ModelProviderGoalUsagePricer::new(Arc::new(
                    std::collections::HashMap::new(),
                ))),
            )
            .unwrap(),
        );
        let result = fixture.run_with_accounting(accounting).await;
        if finite_cost {
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains("Goal cost budget lacks complete pricing"));
            assert_eq!(
                zeroclaw_api::turn_stop::turn_stop(&error).unwrap().code,
                zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted
            );
            assert_eq!(fixture.task().await.status, TaskStatus::Failed);
            assert_eq!(
                fixture.reason().await.as_deref(),
                Some("pricing_unavailable")
            );
        } else {
            assert_eq!(result.unwrap(), GoalExecutionOutcome::Fenced);
            assert_eq!(fixture.task().await.status, TaskStatus::Paused);
        }
        assert_eq!(
            tracker
                .get_usage_totals_for_task(fixture.scope.task_id())
                .unwrap()
                .0,
            7
        );
        assert_eq!(
            std::fs::read(costs.join("state/costs.jsonl")).unwrap(),
            before
        );
        let goal = fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
    }
}

#[tokio::test]
async fn engine_spend_refusal_preserves_committed_operator_pause_and_cancel() {
    for case in [SpendRefusalCase::OperatorPause, SpendRefusalCase::Cancel] {
        let mut fixture =
            EngineFixture::new(&[], &[], Some(ScriptedFault::ParentSpendRefusal(case))).await;
        assert_eq!(fixture.run().await.unwrap(), GoalExecutionOutcome::Fenced);
        let task = fixture.task().await;
        let goal = fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap();
        if matches!(case, SpendRefusalCase::OperatorPause) {
            assert_eq!(task.status, TaskStatus::Paused);
            assert_eq!(goal.pause_reason, Some(GoalPauseReason::OperatorPaused));
        } else {
            assert_eq!(task.status, TaskStatus::Cancelled);
            assert_eq!(fixture.reason().await.as_deref(), Some("operator_cancel"));
        }
        assert_eq!(fixture.steps().len(), 1);
        assert!(fixture.history().is_empty());
    }
}

#[tokio::test]
async fn engine_stale_spend_pause_cas_cannot_abandon_the_same_running_epoch() {
    let mut fixture = EngineFixture::new(
        &[],
        &[],
        Some(ScriptedFault::ParentSpendRefusal(SpendRefusalCase::Clean)),
    )
    .await;
    let connection =
        rusqlite::Connection::open(fixture._directory.path().join("control/control_plane.db"))
            .unwrap();
    connection.execute_batch("CREATE TRIGGER ignore_spend_pause BEFORE UPDATE OF status ON tasks WHEN NEW.status = 'paused' BEGIN SELECT RAISE(IGNORE); END;").unwrap();
    drop(connection);
    let error = fixture.run().await.unwrap_err();
    assert_eq!(
        zeroclaw_api::turn_stop::turn_stop(&error).unwrap().code,
        zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("parent_operation_failed")
    );
    assert_eq!(fixture.steps().len(), 1);
}

/// Interpose only the ordinary-budget pause. Every canonical row and guarded
/// write remains in the existing SQLite owner, including the competing winner.
struct SpendPauseRaceRegistry {
    store: Arc<SqliteTaskStore>,
    replacement: bool,
    fail_winner_read: bool,
    winner_read_pending: AtomicBool,
    pause_calls: AtomicUsize,
    old_pause_result: Mutex<Option<GoalTransitionResult>>,
}

#[async_trait]
impl GoalTaskRegistry for SpendPauseRaceRegistry {
    async fn record_control_intent(
        &self,
        intent: &zeroclaw_goal::task::GoalControlIntent,
    ) -> anyhow::Result<()> {
        self.store.record_control_intent(intent).await
    }
    async fn claim_control_intent(
        &self,
        id: &str,
        boot: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store.claim_control_intent(id, boot).await
    }
    async fn settle_control_intent(
        &self,
        id: &str,
        settlement: zeroclaw_goal::task::GoalControlIntentSettlement,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store.settle_control_intent(id, settlement).await
    }
    async fn latest_active_goal_for_agent(
        &self,
        agent: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        self.store.latest_active_goal_for_agent(agent).await
    }
    async fn latest_active_goal_for_context(
        &self,
        agent: &str,
        route: Option<&str>,
        principal: Option<&str>,
    ) -> anyhow::Result<Option<TaskRecord>> {
        self.store
            .latest_active_goal_for_context(agent, route, principal)
            .await
    }
    async fn latest_active_goal_id_for_context(
        &self,
        agent: &str,
        route: Option<&str>,
        principal: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        self.store
            .latest_active_goal_id_for_context(agent, route, principal)
            .await
    }
    async fn get_goal_task(&self, id: &str) -> anyhow::Result<Option<GoalTaskRecord>> {
        self.store.get_goal_task(id).await
    }
    async fn update_goal_limits(
        &self,
        id: &str,
        tokens: Option<u64>,
        cost: Option<f64>,
    ) -> anyhow::Result<()> {
        self.store.update_goal_limits(id, tokens, cost).await
    }
    async fn update_goal_pause(
        &self,
        id: &str,
        pause: Option<GoalPauseState>,
    ) -> anyhow::Result<()> {
        self.store.update_goal_pause(id, pause).await
    }
    async fn set_continuation_context(
        &self,
        id: &str,
        context: Option<TaskContinuationContext>,
    ) -> anyhow::Result<()> {
        self.store.set_continuation_context(id, context).await
    }
    async fn get_continuation_context(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<TaskContinuationContext>> {
        self.store.get_continuation_context(id).await
    }
    async fn current_goal_for_session(&self, session: &str) -> anyhow::Result<Option<TaskRecord>> {
        if self.winner_read_pending.swap(false, Ordering::SeqCst) {
            anyhow::bail!("fixture post-CAS winning-state read failed");
        }
        self.store.current_goal_for_session(session).await
    }
    async fn terminal_reason_for_session_goal(
        &self,
        id: &str,
        session: &str,
    ) -> anyhow::Result<Option<String>> {
        self.store
            .terminal_reason_for_session_goal(id, session)
            .await
    }
    async fn create_or_replace_session_goal(
        &self,
        task: TaskRecord,
        goal: GoalTaskRecord,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store.create_or_replace_session_goal(task, goal).await
    }
    async fn pause_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        pause: GoalPauseState,
    ) -> anyhow::Result<GoalTransitionResult> {
        if pause.reason != GoalPauseReason::BudgetExhausted {
            return self
                .store
                .pause_session_goal(id, session, epoch, pause)
                .await;
        }
        assert_eq!(self.pause_calls.fetch_add(1, Ordering::SeqCst), 0);
        let mut task = self.store.current_goal_for_session(session).await?.unwrap();
        let mut goal = self.store.get_goal_task(id).await?.unwrap();
        assert_eq!(task.id, id);
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.execution_epoch, epoch);
        assert_eq!(
            self.store
                .finish_session_goal(
                    id,
                    session,
                    epoch,
                    TaskStatus::Cancelled,
                    Some("fixture competing cancel".into())
                )
                .await?,
            GoalTransitionResult::Applied
        );
        if self.replacement {
            task.id = format!("{id}-replacement");
            task.status = TaskStatus::Running;
            task.execution_epoch = 1;
            task.finished_at = None;
            task.heartbeat_at = None;
            task.started_at = chrono::Utc::now().to_rfc3339();
            goal.task_id = task.id.clone();
            assert_eq!(
                self.store
                    .create_or_replace_session_goal(task, goal)
                    .await?,
                GoalTransitionResult::Applied
            );
        }
        // The winner is committed after the engine's precheck but before its
        // real old-epoch CAS, not substituted for the CAS result itself.
        let result = self
            .store
            .pause_session_goal(id, session, epoch, pause)
            .await?;
        assert!(matches!(
            result,
            GoalTransitionResult::Stale | GoalTransitionResult::Missing
        ));
        *self.old_pause_result.lock().unwrap() = Some(result);
        self.winner_read_pending
            .store(self.fail_winner_read, Ordering::SeqCst);
        Ok(result)
    }
    async fn resume_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        pid: u32,
        boot: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store
            .resume_session_goal(id, session, epoch, pid, boot)
            .await
    }
    async fn finish_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        status: TaskStatus,
        error: Option<String>,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store
            .finish_session_goal(id, session, epoch, status, error)
            .await
    }
    async fn admit_pending_operation(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        operation: &str,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store
            .admit_pending_operation(id, session, epoch, operation)
            .await
    }
    async fn settle_pending_operation(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        operation: &str,
        state: GoalAccountingState,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store
            .settle_pending_operation(id, session, epoch, operation, state)
            .await
    }
    async fn update_session_goal_limits(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
        tokens: Option<u64>,
        cost: Option<f64>,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store
            .update_session_goal_limits(id, session, epoch, tokens, cost)
            .await
    }
    async fn delete_session_goal(
        &self,
        id: &str,
        session: &str,
        epoch: i64,
    ) -> anyhow::Result<GoalTransitionResult> {
        self.store.delete_session_goal(id, session, epoch).await
    }
}

#[tokio::test]
async fn engine_spend_pause_cas_loses_to_committed_cancel_or_replacement_and_reconciles_exact_winner()
 {
    for replacement in [false, true] {
        for fail_winner_read in [false, true] {
            let mut fixture = EngineFixture::new(
                &[],
                &[],
                Some(ScriptedFault::ParentSpendRefusal(SpendRefusalCase::Clean)),
            )
            .await;
            let registry = Arc::new(SpendPauseRaceRegistry {
                store: Arc::clone(&fixture.store),
                replacement,
                fail_winner_read,
                winner_read_pending: AtomicBool::new(false),
                pause_calls: AtomicUsize::new(0),
                old_pause_result: Mutex::new(None),
            });
            fixture.runtime = GoalRuntime::new(registry.clone());
            let result = fixture.run().await;
            if fail_winner_read {
                let error = result.unwrap_err();
                assert_eq!(
                    zeroclaw_api::turn_stop::turn_stop(&error).unwrap().code,
                    zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted
                );
                let text = format!("{error:#}");
                assert!(text.contains("fixture post-CAS winning-state read failed"));
                assert_eq!(
                    text.matches("fixture ordinary daily spend refused").count(),
                    1
                );
            } else {
                assert_eq!(result.unwrap(), GoalExecutionOutcome::Fenced);
            }
            assert_eq!(registry.pause_calls.load(Ordering::SeqCst), 1);
            assert!(matches!(
                *registry.old_pause_result.lock().unwrap(),
                Some(GoalTransitionResult::Stale | GoalTransitionResult::Missing)
            ));
            let winner = fixture.task().await;
            if replacement {
                assert_eq!(
                    winner.id,
                    format!("{}-replacement", fixture.scope.task_id())
                );
                assert_eq!(winner.status, TaskStatus::Running);
                assert_eq!(winner.execution_epoch, 1);
                let goal = fixture
                    .store
                    .get_goal_task(&winner.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(goal.pause_reason.is_none());
                assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
            } else {
                assert_eq!(winner.id, fixture.scope.task_id());
                assert_eq!(winner.status, TaskStatus::Cancelled);
                assert_eq!(
                    fixture.reason().await.as_deref(),
                    Some("fixture competing cancel")
                );
            }
            assert_eq!(fixture.steps().len(), 1);
            assert!(fixture.history().is_empty());
            assert!(
                fixture
                    .driver
                    .script
                    .lock()
                    .unwrap()
                    .published_core_errors
                    .is_empty()
            );
        }
    }
}

#[tokio::test]
async fn engine_spend_pause_sql_and_completeness_read_errors_retain_the_native_refusal() {
    for bad_read in [false, true] {
        let case = if bad_read {
            SpendRefusalCase::BadAccountingRead
        } else {
            SpendRefusalCase::Clean
        };
        let mut fixture =
            EngineFixture::new(&[], &[], Some(ScriptedFault::ParentSpendRefusal(case))).await;
        if !bad_read {
            let connection = rusqlite::Connection::open(
                fixture._directory.path().join("control/control_plane.db"),
            )
            .unwrap();
            connection.execute_batch("CREATE TRIGGER fail_spend_pause BEFORE UPDATE OF status ON tasks WHEN NEW.status = 'paused' BEGIN SELECT RAISE(FAIL, 'fixture spend pause SQL failure'); END;").unwrap();
        }
        let error = fixture.run().await.unwrap_err();
        assert_eq!(
            zeroclaw_api::turn_stop::turn_stop(&error).unwrap().code,
            zeroclaw_api::turn_stop::TurnStopCode::BudgetExhausted
        );
        let text = format!("{error:#}");
        assert!(
            text.contains(if bad_read {
                "get goal task"
            } else {
                "fixture spend pause SQL failure"
            }),
            "{text}"
        );
        assert_eq!(
            text.matches("fixture ordinary daily spend refused").count(),
            1,
            "{text}"
        );
        // Public run preserves the native failure; its supervisor is the
        // separate fallback owner that terminalizes an unexpected error.
        assert_eq!(fixture.task().await.status, TaskStatus::Running);
        assert_eq!(fixture.steps().len(), 1);
    }
}

#[tokio::test]
async fn engine_incomplete_accounting_names_the_durable_condition() {
    // A successful parent over durable Invalid accounting cannot append or verify.
    let mut fixture =
        EngineFixture::new(&["candidate"], &[], Some(ScriptedFault::SettleIncomplete)).await;
    let error = fixture.run().await.unwrap_err();
    assert!(error.to_string().contains("Goal accounting is incomplete"));
    assert!(error.to_string().contains("invalid or could not be priced"));
    assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
}

#[tokio::test]
async fn engine_empty_candidate_over_incomplete_accounting_fails_as_accounting() {
    // Accounting failure takes precedence over an empty parent response.
    let mut fixture =
        EngineFixture::new(&["   "], &[], Some(ScriptedFault::SettleIncomplete)).await;
    let error = fixture.run().await.unwrap_err();
    assert!(error.downcast_ref::<GoalAccountingFailure>().is_some());
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert!(fixture.history().is_empty());
}

#[tokio::test]
async fn engine_empty_candidate_fails_without_append_or_verifier() {
    // Complete accounting does not make an empty response a candidate.
    let mut fixture = EngineFixture::new(&["   "], &[], None).await;
    let error = fixture.run().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Goal parent returned an empty candidate")
    );
    assert_eq!(fixture.reason().await.as_deref(), Some("candidate_empty"));
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert!(fixture.history().is_empty());
}

#[tokio::test]
async fn engine_parent_operation_error_keeps_its_classification() {
    // Ordinary provider errors retain their operation failure reason and chain.
    let mut fixture = EngineFixture::new(&[], &[], Some(ScriptedFault::ParentError)).await;
    let error = fixture.run().await.unwrap_err();
    assert!(format!("{error:#}").contains("Goal parent operation failed"));
    assert!(format!("{error:#}").contains("fixture provider failed"));
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("parent_operation_failed")
    );
}

#[tokio::test]
async fn engine_verifier_accounting_failure_fails_as_accounting_after_the_candidate_is_durable() {
    // Verifier accounting failure leaves the already appended parent response.
    let mut fixture = EngineFixture::new(
        &["final answer"],
        &[],
        Some(ScriptedFault::VerifierAccountingFailure),
    )
    .await;
    let error = fixture.run().await.unwrap_err();
    assert!(format!("{error:#}").contains("fixture pricing failed"));
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "final answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("final answer".into()),
        ]
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("accounting_missing_or_invalid")
    );
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "final answer".into())]
    );
}

#[tokio::test]
async fn engine_verifier_operation_error_keeps_its_classification() {
    // Ordinary verifier errors remain distinct from typed accounting failures.
    let mut fixture =
        EngineFixture::new(&["answer"], &[], Some(ScriptedFault::VerifierError)).await;
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("Goal verifier operation failed"));
    assert!(rendered.contains("fixture verifier failed"));
    assert!(
        !error
            .chain()
            .any(|cause| cause.is::<GoalAccountingFailure>())
    );
    assert_eq!(fixture.task().await.status, TaskStatus::Failed);
    assert_eq!(
        fixture.reason().await.as_deref(),
        Some("verifier_operation_failed")
    );
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append(
                "answer".into(),
                TaskStatus::Running,
                fixture.scope.execution_epoch()
            ),
            ScriptedStep::Verifier("answer".into()),
        ]
    );
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_verifier_error_after_a_winning_pause_keeps_both_errors_and_the_pause() {
    // A verifier failure cannot overwrite a committed pause or lose its cause.
    let mut fixture = EngineFixture::new(
        &["answer"],
        &[],
        Some(ScriptedFault::PauseThenVerifierError),
    )
    .await;
    let epoch = fixture.scope.execution_epoch();
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture verifier failed"));
    assert!(rendered.contains("Goal verifier operation failed"));
    assert!(rendered.contains("Goal failure lost its execution fence"));
    assert_eq!(
        fixture.steps(),
        vec![
            ScriptedStep::Parent(Vec::new()),
            ScriptedStep::Append("answer".into(), TaskStatus::Running, epoch),
            ScriptedStep::Verifier("answer".into()),
        ]
    );
    let task = fixture.task().await;
    assert_eq!(task.status, TaskStatus::Paused);
    assert_eq!(task.execution_epoch, epoch + 1);
    assert_eq!(
        fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap()
            .pause_reason,
        Some(GoalPauseReason::OperatorPaused)
    );
    assert!(fixture.reason().await.is_none());
    assert_eq!(
        fixture.history(),
        vec![("assistant".into(), "answer".into())]
    );
}

#[tokio::test]
async fn engine_parent_error_after_a_winning_pause_keeps_both_errors_and_the_pause() {
    // A failed stale-epoch terminal write preserves both errors and the pause.
    let mut fixture = EngineFixture::new(&[], &[], Some(ScriptedFault::PauseThenParentError)).await;
    let epoch = fixture.scope.execution_epoch();
    let error = fixture.run().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("fixture provider failed"));
    assert!(rendered.contains("Goal parent operation failed"));
    assert!(rendered.contains("Goal failure lost its execution fence"));
    assert_eq!(fixture.steps(), vec![ScriptedStep::Parent(Vec::new())]);
    let task = fixture.task().await;
    assert_eq!(task.status, TaskStatus::Paused);
    assert_eq!(task.execution_epoch, epoch + 1);
    assert_eq!(
        fixture
            .store
            .get_goal_task(fixture.scope.task_id())
            .await
            .unwrap()
            .unwrap()
            .pause_reason,
        Some(GoalPauseReason::OperatorPaused)
    );
    assert!(fixture.reason().await.is_none());
    assert!(fixture.history().is_empty());
}

#[tokio::test]
async fn execution_lease_must_match_trusted_ingress_after_revalidation() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(WrongExecutionLeaseDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        returned_session_key: GoalSessionKey::zero_code("wrong-session").unwrap(),
        execution_acquires: AtomicUsize::new(0),
    });
    let scope = GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    let checkpoint = GoalParentTurnCheckpoint::new();
    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
    {
        Ok(_) => panic!("mismatched execution lease must be rejected"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("lease session key"));
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disabled_goal_mode_cannot_acquire_an_execution_lease() {
    let config = valid_goal_config(false);
    let enabled_config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let scope = GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&enabled_config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    let checkpoint = GoalParentTurnCheckpoint::new();
    let error = match host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
    {
        Ok(_) => panic!("disabled Goal Mode must not acquire an execution lease"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("disabled"));
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exact_driver_binding_and_typed_command_are_preserved() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });
    let command = GoalCommand::Pause { immediate: false };

    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            command.clone(),
        )
        .await
        .unwrap();

    assert_eq!(submission.ingress(), &ingress);
    assert_eq!(submission.command(), &command);
    assert_eq!(driver.binds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn submission_debug_does_not_expose_goal_text_or_raw_principals() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            recording_driver(&ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "private stop condition".into(),
            },
        )
        .await
        .unwrap();

    let debug = format!("{submission:?}");
    assert!(debug.contains("surface: Matrix"));
    assert!(debug.contains("command_kind: \"start\""));
    assert!(!debug.contains("private stop condition"));
    assert!(!debug.contains("@alice:example.org"));

    let ingress_debug = format!("{ingress:?}");
    assert!(!ingress_debug.contains("@alice:example.org"));
    let scope_debug = format!(
        "{:?}",
        GoalExecutionScope::new(
            "goal-1",
            "matrix_room__room_example_org__alice_example_org",
            1
        )
        .unwrap()
    );
    assert!(!scope_debug.contains("alice_example_org"));
    let parent_debug = format!(
        "{:?}",
        GoalParentTurn {
            kind: GoalParentTurnKind::Start,
            objective: "private stop condition".into(),
            working_history: Vec::new(),
        }
    );
    assert!(!parent_debug.contains("private stop condition"));
    let verifier_debug = format!(
        "{:?}",
        GoalVerifierTurn {
            objective: "private stop condition".into(),
            candidate: "private candidate".into(),
        }
    );
    assert!(!verifier_debug.contains("private stop condition"));
    assert!(!verifier_debug.contains("private candidate"));
}

#[test]
fn identical_raw_ids_from_matrix_and_zerocode_are_not_the_same_session() {
    let matrix = GoalSessionKey::matrix("matrix_shared-id").unwrap();
    let zerocode = GoalSessionKey::zero_code("shared-id").unwrap();

    assert_ne!(matrix, zerocode);
    assert_ne!(matrix.durable_id(), zerocode.durable_id());
}

#[test]
fn matrix_history_key_must_already_use_the_canonical_session_form() {
    let raw = "matrix_room:!room:example.org:@alice:example.org";

    assert!(GoalSessionKey::matrix(raw).is_err());
    assert!(GoalSessionKey::matrix(" matrix_room").is_err());
    assert!(GoalSessionKey::zero_code(" same-session").is_err());
}

#[test]
fn trusted_ingress_rejects_invalid_or_cross_surface_authority() {
    let matrix_key =
        GoalSessionKey::matrix("matrix_room__room_example_org__alice_example_org").unwrap();
    let matrix_principal = GoalIngressPrincipal::Matrix {
        raw_mxid: "@alice:example.org".into(),
    };

    assert!(
        GoalIngressContext::trusted(
            matrix_key.clone(),
            "",
            "matrix:primary",
            matrix_principal.clone(),
        )
        .is_err()
    );
    assert!(
        GoalIngressContext::trusted(matrix_key.clone(), "main", "", matrix_principal.clone(),)
            .is_err()
    );
    assert!(
        GoalIngressContext::trusted(
            matrix_key.clone(),
            "main",
            "matrix:primary",
            GoalIngressPrincipal::Matrix {
                raw_mxid: " ".into(),
            },
        )
        .is_err()
    );
    assert!(
        GoalIngressContext::trusted(
            matrix_key,
            "main",
            "matrix:primary",
            GoalIngressPrincipal::ZeroCode {
                tui_id: "connection".into(),
            },
        )
        .is_err()
    );
}

#[test]
fn trusted_ingress_rejects_noncanonical_principal_identity() {
    let session_key =
        GoalSessionKey::matrix("matrix_room__room_example_org__alice_example_org").unwrap();

    let error = GoalIngressContext::trusted(
        session_key,
        "main",
        "matrix:primary",
        GoalIngressPrincipal::Matrix {
            raw_mxid: " @alice:example.org ".into(),
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("not canonical"));
}

#[test]
fn host_settings_do_not_cache_config_debug_content() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let settings = GoalHostSettings::from_config(&config, 42, "test-boot").unwrap();
    let rendered = format!("{settings:?}");
    assert!(!rendered.contains("gpt-test"));
    assert!(!rendered.contains("openai.default"));
}

#[test]
fn invalid_and_zero_only_default_limits_do_not_admit_a_budget() {
    for (tokens, cost) in [
        (Some(i64::MAX as u64 + 1), None),
        (None, Some(-1.0)),
        (None, Some(f64::NAN)),
    ] {
        let mut config = valid_goal_config(true);
        config.goal.default_token_limit = tokens;
        config.goal.default_cost_limit_usd = cost;
        assert!(config.goal.validate().is_err());
    }
    let mut config = valid_goal_config(true);
    config.goal.default_token_limit = Some(0);
    config.goal.default_cost_limit_usd = None;
    assert!(matches!(
        config.goal.default_budget().unwrap(),
        zeroclaw_config::goal::GoalDefaultBudget::Unconfigured
    ));
}

#[test]
fn host_settings_rejects_a_blank_boot_id() {
    let config = goal_config(
        true,
        "openai.default",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    assert!(GoalHostSettings::from_config(&config, 42, " \t\n").is_err());
}

#[test]
fn execution_scope_rejects_blank_ids_and_nonpositive_epochs() {
    assert!(GoalExecutionScope::new("", "matrix_session", 1).is_err());
    assert!(GoalExecutionScope::new("goal-1", "", 1).is_err());
    assert!(GoalExecutionScope::new(" goal-1", "matrix_session", 1).is_err());
    assert!(GoalExecutionScope::new("goal-1", "matrix_session ", 1).is_err());
    assert!(GoalExecutionScope::new("goal-1", "matrix_session", 0).is_err());
}

#[tokio::test]
async fn controller_submission_future_is_send() {
    fn assert_send<T: Send>(_: T) {}

    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            recording_driver(&ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    assert_send(controller.submit(&host_settings(&config), &submission));
}

#[tokio::test]
async fn controller_uses_only_a_host_validated_submission_for_lifecycle_transitions() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = Arc::new(RecordingDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        binds: AtomicUsize::new(0),
        execution_acquires: AtomicUsize::new(0),
    });
    let host = GoalExecutionHost::new();

    let start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "stop when the implementation is complete".into(),
            },
        )
        .await
        .unwrap();
    let (response, _start) = controller
        .submit_for_execution(&host_settings(&config), start)
        .await
        .unwrap();
    let GoalResponse::Started(started) = response else {
        panic!("expected a started Goal");
    };
    assert_eq!(started.execution_epoch, 1);
    assert_eq!(started.token_limit, Some(100));
    assert_eq!(started.cost_limit_usd, Some(1.0));
    let stored_task = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .expect("the started Goal must be current");
    let stored_goal = store
        .get_goal_task(&started.task_id)
        .await
        .unwrap()
        .expect("the started Goal extension must exist");
    assert_eq!(
        stored_task.principal_id.as_deref(),
        Some("@alice:example.org")
    );
    assert_eq!(
        stored_goal.objective,
        "stop when the implementation is complete"
    );

    let pause = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();
    let GoalResponse::Paused(paused) = controller
        .submit(&host_settings(&config), &pause)
        .await
        .unwrap()
    else {
        panic!("expected a paused Goal");
    };
    assert_eq!(paused.execution_epoch, 2);

    let resume = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    let GoalResponse::Resumed(resumed) = controller
        .submit(&host_settings(&config), &resume)
        .await
        .unwrap()
    else {
        panic!("expected a resumed Goal");
    };
    assert_eq!(resumed.execution_epoch, 3);

    let cancel = host
        .submit(
            &host_settings(&config),
            ingress,
            driver,
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));
}

#[tokio::test]
async fn controller_distinguishes_active_goals_from_terminal_goals_with_unsettled_operations() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();

    let start_command = || GoalCommand::Start {
        budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
        objective: "finish the assigned task".into(),
    };
    let start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            start_command(),
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &start)
        .await
        .unwrap()
    else {
        panic!("expected a started Goal");
    };

    let active_start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            start_command(),
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &active_start)
            .await
            .unwrap(),
        GoalResponse::AlreadyActive
    ));

    assert_eq!(
        store
            .admit_pending_operation(
                &started.task_id,
                &ingress.session_key().durable_id(),
                started.execution_epoch,
                "operation-under-settlement",
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let cancel = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));

    let terminal_start = host
        .submit(&host_settings(&config), ingress, driver, start_command())
        .await
        .unwrap();
    let GoalResponse::Terminal(terminal) = controller
        .submit(&host_settings(&config), &terminal_start)
        .await
        .unwrap()
    else {
        panic!("a terminal Goal with an unsettled operation must not be called active");
    };
    assert_eq!(terminal.status, TaskStatus::Cancelled);
    assert!(!terminal.resumable);
}

#[tokio::test]
async fn resume_projects_a_paused_goal_that_is_not_yet_resumable() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();

    let start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the assigned task".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &start)
        .await
        .unwrap()
    else {
        panic!("expected a started Goal");
    };
    assert_eq!(
        store
            .admit_pending_operation(
                &started.task_id,
                &ingress.session_key().durable_id(),
                started.execution_epoch,
                "operation-still-settling",
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );

    // Only an immediate operator pause may fence this unsettled operation;
    // plain pause belongs to the supervisor's settled-turn checkpoint.
    let pause = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &pause)
            .await
            .unwrap(),
        GoalResponse::Paused(_)
    ));

    let resume = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    let GoalResponse::Status(status) = controller
        .submit(&host_settings(&config), &resume)
        .await
        .unwrap()
    else {
        panic!("resume must project the durable non-resumable paused state");
    };
    assert_eq!(status.status, TaskStatus::Paused);
    assert!(!status.resumable);

    assert_eq!(
        store
            .settle_pending_operation(
                &started.task_id,
                &ingress.session_key().durable_id(),
                started.execution_epoch,
                "operation-still-settling",
                GoalAccountingState::Invalid,
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );

    let resume = host
        .submit(
            &host_settings(&config),
            ingress,
            driver,
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    let GoalResponse::Status(status) = controller
        .submit(&host_settings(&config), &resume)
        .await
        .unwrap()
    else {
        panic!("incomplete accounting must project a durable non-resumable paused state");
    };
    assert_eq!(status.status, TaskStatus::Paused);
    assert_eq!(status.accounting_state, GoalAccountingState::Invalid);
    assert!(!status.resumable);
}

#[tokio::test]
async fn controller_projects_each_command_state_without_reinterpreting_the_store() {
    let enabled_config = valid_goal_config(true);
    let disabled_config = valid_goal_config(false);
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let host = GoalExecutionHost::new();

    let help = host
        .submit(
            &host_settings(&disabled_config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Help,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&disabled_config), &help)
            .await
            .unwrap(),
        GoalResponse::Disabled
    ));
    let disabled_status = host
        .submit(
            &host_settings(&disabled_config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&disabled_config), &disabled_status)
            .await
            .unwrap(),
        GoalResponse::Disabled
    ));

    let absent = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &absent)
            .await
            .unwrap(),
        GoalResponse::NoCurrentGoal
    ));

    let start = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the assigned task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &start)
            .await
            .unwrap(),
        GoalResponse::Started(_)
    ));

    for (command, expected) in [
        (GoalCommand::Status, "status"),
        (GoalCommand::Budget, "budget"),
    ] {
        let submission = host
            .submit(
                &host_settings(&enabled_config),
                ingress.clone(),
                driver.clone(),
                command,
            )
            .await
            .unwrap();
        let response = controller
            .submit(&host_settings(&enabled_config), &submission)
            .await
            .unwrap();
        assert!(
            matches!(
                (&response, expected),
                (GoalResponse::Status(_), "status") | (GoalResponse::Budget(_), "budget")
            ),
            "unexpected response for {expected}: {response:?}"
        );
    }

    let update = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::SetBudget(zeroclaw_commands::goal::GoalBudgetSelection::Limits(
                zeroclaw_commands::goal::GoalBudgetLimits {
                    token_limit: Some(7),
                    cost_limit_usd: None,
                },
            )),
        )
        .await
        .unwrap();
    let GoalResponse::BudgetUpdated(updated) = controller
        .submit(&host_settings(&enabled_config), &update)
        .await
        .unwrap()
    else {
        panic!("expected the guarded budget update projection");
    };
    assert_eq!(updated.token_limit, Some(7));
    assert_eq!(updated.cost_limit_usd, None);

    let pause = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &pause)
            .await
            .unwrap(),
        GoalResponse::Paused(_)
    ));
    let pause_again = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &pause_again)
            .await
            .unwrap(),
        GoalResponse::AlreadyPaused(_)
    ));

    let cancel = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));
    let cancel_again = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &cancel_again)
            .await
            .unwrap(),
        GoalResponse::AlreadyCancelled(_)
    ));
    let terminal_status = host
        .submit(
            &host_settings(&config),
            ingress,
            driver,
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&enabled_config), &terminal_status)
            .await
            .unwrap(),
        GoalResponse::Terminal(_)
    ));
}

#[tokio::test]
async fn local_help_does_not_bind_a_live_session_when_goal_mode_is_enabled() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);

    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Help,
        )
        .await
        .unwrap();

    assert_eq!(submission.command(), &GoalCommand::Help);
    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &submission)
            .await
            .unwrap(),
        GoalResponse::Help
    ));
}

#[tokio::test]
async fn runtime_does_not_create_execution_for_read_only_goal_commands() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store as Arc<dyn GoalTaskRegistry>);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: Arc::new(AtomicUsize::new(0)),
    });

    let (response, execution) = runtime
        .submit(&settings, ingress, driver.clone(), GoalCommand::Status)
        .await
        .unwrap()
        .into_parts();
    assert!(matches!(response, GoalResponse::NoCurrentGoal));
    assert!(execution.is_none());
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runtime_start_without_budget_defaults_returns_budget_required_without_execution() {
    use zeroclaw_commands::goal::{GoalBudgetLimits, GoalBudgetSelection};

    // Missing defaults stop admission without handing off execution; explicit limits admit it.
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let session_id = ingress.session_key().durable_id();
        let mut config = valid_goal_config(true);
        config.goal.default_token_limit = None;
        config.goal.default_cost_limit_usd = None;
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let settings = host_settings(&config);
        let driver = Arc::new(ExecutionDriver {
            binding: GoalSessionBinding::new(ingress.session_key().clone()),
            execution_acquires: AtomicUsize::new(0),
            delivered: Arc::new(AtomicUsize::new(0)),
        });

        let (response, execution) = runtime
            .submit(
                &settings,
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "start without configured budget defaults".into(),
                },
            )
            .await
            .unwrap()
            .into_parts();
        assert!(matches!(response, GoalResponse::BudgetRequired));
        assert!(execution.is_none());
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
        assert!(
            store
                .current_goal_for_session(&session_id)
                .await
                .unwrap()
                .is_none()
        );

        let (response, execution) = runtime
            .submit(
                &settings,
                ingress,
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Limits(GoalBudgetLimits {
                        token_limit: Some(10),
                        cost_limit_usd: None,
                    }),
                    objective: "start with explicit budget limits".into(),
                },
            )
            .await
            .unwrap()
            .into_parts();
        assert!(matches!(response, GoalResponse::Started(_)));
        assert!(execution.is_some());
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn runtime_rejects_dangling_verifier_for_start_and_resume_but_allows_status() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let invalid = goal_config(
        true,
        "openai.missing",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );

    for command in [
        GoalCommand::Start {
            budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
            objective: "must not start".into(),
        },
        GoalCommand::Resume { response: None },
    ] {
        let error = match runtime
            .submit(
                &host_settings(&invalid),
                ingress.clone(),
                driver.clone(),
                command,
            )
            .await
        {
            Ok(_) => panic!("invalid verifier must reject Start and Resume"),
            Err(error) => error,
        };
        assert_admission_error(
            error,
            ConfigApiCode::DanglingReference,
            "goal.verifier.model_provider",
        );
    }

    assert_eq!(driver.binds.load(Ordering::SeqCst), 0);
    assert!(
        store
            .current_goal_for_session(&ingress.session_key().durable_id())
            .await
            .unwrap()
            .is_none(),
        "rejected Start and Resume must not create a goal row"
    );

    let (response, execution) = runtime
        .submit(
            &host_settings(&invalid),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap()
        .into_parts();
    assert!(matches!(response, GoalResponse::NoCurrentGoal));
    assert!(execution.is_none());
    assert_eq!(driver.binds.load(Ordering::SeqCst), 1);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runtime_rechecks_dangling_verifier_before_execution_acquisition() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: Arc::new(AtomicUsize::new(0)),
    });
    let valid = valid_goal_config(true);
    let (response, request) = runtime
        .submit(
            &host_settings(&valid),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "acquire only while the verifier remains configured".into(),
            },
        )
        .await
        .unwrap()
        .into_parts();
    assert!(matches!(response, GoalResponse::Started(_)));
    let request = match request {
        Some(request) => request,
        None => panic!("a valid Start must return its execution request"),
    };

    let invalid = goal_config(
        true,
        "openai.missing",
        ConfigGoalBudgetLimits {
            token_limit: Some(100),
            cost_limit_usd: Some(1.0),
        },
    );
    let error = match runtime
        .acquire_execution(
            &host_settings(&invalid),
            request,
            &GoalParentTurnCheckpoint::new(),
        )
        .await
    {
        Ok(_) => panic!("dangling verifier must refuse execution acquisition"),
        Err(error) => error,
    };
    assert_admission_error(
        error,
        ConfigApiCode::DanglingReference,
        "goal.verifier.model_provider",
    );
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    // Settlement after a refused acquisition belongs to the later executor.

    let fresh_store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let fresh_runtime = GoalRuntime::new(fresh_store as Arc<dyn GoalTaskRegistry>);
    let fresh_ingress = matrix_ingress();
    let fresh_driver = Arc::new(ExecutionDriver {
        binding: GoalSessionBinding::new(fresh_ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        delivered: Arc::new(AtomicUsize::new(0)),
    });
    let fresh_valid = valid_goal_config(true);
    let (response, request) = fresh_runtime
        .submit(
            &host_settings(&fresh_valid),
            fresh_ingress,
            fresh_driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "acquire with fresh valid settings".into(),
            },
        )
        .await
        .unwrap()
        .into_parts();
    assert!(matches!(response, GoalResponse::Started(_)));
    let request = match request {
        Some(request) => request,
        None => panic!("a valid Start must return its execution request"),
    };
    let acquisition_settings = host_settings(&fresh_valid);
    let lease = fresh_runtime
        .acquire_execution(
            &acquisition_settings,
            request,
            &GoalParentTurnCheckpoint::new(),
        )
        .await
        .unwrap();
    drop(lease);
    assert_eq!(fresh_driver.execution_acquires.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn same_zerocode_session_retains_goal_control_after_agent_alias_refresh() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let host = GoalExecutionHost::new();
    let first_ingress = zerocode_ingress("alpha");
    let first_driver = recording_driver(&first_ingress);

    let start = host
        .submit(
            &host_settings(&config),
            first_ingress,
            first_driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the assigned task".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started) = controller
        .submit(&host_settings(&config), &start)
        .await
        .unwrap()
    else {
        panic!("expected a started Goal");
    };
    let stored_task = store
        .current_goal_for_session("rpc_same-live-session")
        .await
        .unwrap()
        .expect("the started Goal must be current");
    assert_eq!(stored_task.id, started.task_id);
    assert_eq!(stored_task.principal_id, None);

    let refreshed_ingress = zerocode_ingress("beta");
    let refreshed_driver = recording_driver(&refreshed_ingress);
    let status = host
        .submit(
            &host_settings(&config),
            refreshed_ingress.clone(),
            refreshed_driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &status)
            .await
            .unwrap(),
        GoalResponse::Status(_)
    ));

    let cancel = host
        .submit(
            &host_settings(&config),
            refreshed_ingress.clone(),
            refreshed_driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));

    let replacement = host
        .submit(
            &host_settings(&config),
            refreshed_ingress,
            refreshed_driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the reassigned task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &replacement)
            .await
            .unwrap(),
        GoalResponse::Started(_)
    ));
}

#[tokio::test]
async fn matrix_goal_control_requires_the_exact_raw_principal_after_key_normalization() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let shared_key = "matrix__r_example_org__a_b_example_org";
    let first_ingress = GoalIngressContext::trusted(
        GoalSessionKey::matrix(shared_key).unwrap(),
        "main",
        "matrix:primary",
        GoalIngressPrincipal::Matrix {
            raw_mxid: "@a.b:example.org".into(),
        },
    )
    .unwrap();
    let colliding_ingress = GoalIngressContext::trusted(
        GoalSessionKey::matrix(shared_key).unwrap(),
        "main",
        "matrix:primary",
        GoalIngressPrincipal::Matrix {
            raw_mxid: "@a_b:example.org".into(),
        },
    )
    .unwrap();
    let host = GoalExecutionHost::new();

    let first_start = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the assigned task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_start)
            .await
            .unwrap(),
        GoalResponse::Started(_)
    ));

    let colliding_status = host
        .submit(
            &host_settings(&config),
            colliding_ingress.clone(),
            recording_driver(&colliding_ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &colliding_status)
            .await
            .unwrap(),
        GoalResponse::NoCurrentGoal
    ));
    let colliding_cancel = host
        .submit(
            &host_settings(&config),
            colliding_ingress.clone(),
            recording_driver(&colliding_ingress),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &colliding_cancel)
            .await
            .unwrap(),
        GoalResponse::NoCurrentGoal
    ));
    let colliding_start = host
        .submit(
            &host_settings(&config),
            colliding_ingress.clone(),
            recording_driver(&colliding_ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish a different task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &colliding_start)
            .await
            .unwrap(),
        GoalResponse::NoCurrentGoal
    ));

    let first_cancel = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_cancel)
            .await
            .unwrap(),
        GoalResponse::Cancelled(_)
    ));

    let colliding_terminal_replacement = host
        .submit(
            &host_settings(&config),
            colliding_ingress.clone(),
            recording_driver(&colliding_ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "replace another principal's terminal goal".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &colliding_terminal_replacement)
            .await
            .unwrap(),
        GoalResponse::NoCurrentGoal
    ));

    let first_terminal_status = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_terminal_status)
            .await
            .unwrap(),
        GoalResponse::Terminal(_)
    ));

    let first_terminal_replacement = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "replace the owner's terminal goal".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_terminal_replacement)
            .await
            .unwrap(),
        GoalResponse::Started(_)
    ));

    let first_replacement_status = host
        .submit(
            &host_settings(&config),
            first_ingress.clone(),
            recording_driver(&first_ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &first_replacement_status)
            .await
            .unwrap(),
        GoalResponse::Status(_)
    ));
}

#[tokio::test]
async fn typed_start_objective_is_rejected_before_durable_write() {
    let config = valid_goal_config(true);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            recording_driver(&ingress),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "x".repeat(zeroclaw_commands::goal::MAX_GOAL_OBJECTIVE_CHARS + 1),
            },
        )
        .await
        .unwrap();

    assert!(
        controller
            .submit(&host_settings(&config), &submission)
            .await
            .is_err()
    );
    assert!(
        store
            .current_goal_for_session(&ingress.session_key().durable_id())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn disabled_goal_submission_does_not_acquire_a_session_lease() {
    let config = valid_goal_config(false);
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let controller = GoalController::new(store as Arc<dyn GoalTaskRegistry>);
    let ingress = matrix_ingress();
    let driver = Arc::new(LeaseDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        available_for_reconnect: Arc::new(AtomicUsize::new(1)),
    });

    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    assert!(
        driver.reconnect_is_allowed(),
        "disabled Goal Mode must not contend the driver's session lease"
    );
    assert!(matches!(
        controller
            .submit(&host_settings(&config), &submission)
            .await
            .unwrap(),
        GoalResponse::Disabled
    ));
    assert!(
        driver.reconnect_is_allowed(),
        "a disabled submission carries no live-session lease"
    );
    drop(submission);
    assert!(
        driver.reconnect_is_allowed(),
        "the host never acquired a lease for disabled Goal Mode"
    );
}

#[tokio::test]
async fn session_lease_stays_held_during_a_guarded_lifecycle_mutation() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let reconnect_available = Arc::new(AtomicUsize::new(1));
    let registry = Arc::new(LeaseObservingRegistry::running_for(
        &ingress,
        reconnect_available.clone(),
    ));
    let controller = GoalController::new(registry.clone() as Arc<dyn GoalTaskRegistry>);
    let driver = Arc::new(LeaseDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        available_for_reconnect: reconnect_available,
    });

    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Pause { immediate: false },
        )
        .await
        .unwrap();

    let GoalResponse::Paused(_) = controller
        .submit(&host_settings(&config), &submission)
        .await
        .unwrap()
    else {
        panic!("expected a paused Goal");
    };
    assert_eq!(registry.pause_observed.load(Ordering::SeqCst), 1);
    assert!(
        !driver.reconnect_is_allowed(),
        "the borrowed submission must retain its exact driver proof"
    );
    drop(submission);
    assert!(
        driver.reconnect_is_allowed(),
        "the lease may release after the guarded lifecycle submission is settled"
    );
}

async fn status_for_registry(
    config: &GoalHostSettings<'_>,
    ingress: &GoalIngressContext,
    registry: Arc<LeaseObservingRegistry>,
) -> GoalResponse {
    let controller = GoalController::new(registry as Arc<dyn GoalTaskRegistry>);
    let submission = GoalExecutionHost::new()
        .submit(
            config,
            ingress.clone(),
            recording_driver(ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    controller.submit(config, &submission).await.unwrap()
}

#[tokio::test]
async fn status_projection_is_stale_after_same_id_resume_changes_status_and_epoch() {
    let config = valid_goal_config(true);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let registry = Arc::new(LeaseObservingRegistry::paused_with_change_after_goal_read(
        &ingress,
        TaskStatus::Running,
        2,
    ));

    assert!(matches!(
        status_for_registry(&settings, &ingress, registry).await,
        GoalResponse::Stale
    ));
}

#[tokio::test]
async fn status_projection_is_stale_after_same_id_terminal_status_change() {
    let config = valid_goal_config(true);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let registry = Arc::new(LeaseObservingRegistry::paused_with_change_after_goal_read(
        &ingress,
        TaskStatus::Cancelled,
        1,
    ));

    assert!(matches!(
        status_for_registry(&settings, &ingress, registry).await,
        GoalResponse::Stale
    ));
}

#[tokio::test]
async fn status_projection_is_stale_after_same_id_epoch_only_change() {
    let config = valid_goal_config(true);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let registry = Arc::new(LeaseObservingRegistry::paused_with_change_after_goal_read(
        &ingress,
        TaskStatus::Paused,
        2,
    ));

    assert!(matches!(
        status_for_registry(&settings, &ingress, registry).await,
        GoalResponse::Stale
    ));
}

#[tokio::test]
async fn status_projection_preserves_stable_running_and_paused_snapshots() {
    let config = valid_goal_config(true);
    let settings = host_settings(&config);
    let ingress = matrix_ingress();
    let mut running_fixture =
        LeaseObservingRegistry::running_for(&ingress, Arc::new(AtomicUsize::new(1)));
    running_fixture.goal.effective_token_limit = Some(100);
    running_fixture.goal.effective_cost_limit_usd = Some(0.75);
    let running = Arc::new(running_fixture);

    let GoalResponse::Status(running) = status_for_registry(&settings, &ingress, running).await
    else {
        panic!("stable running task should project as status");
    };
    assert_eq!(running.status, TaskStatus::Running);
    assert_eq!(running.execution_epoch, 1);
    assert_eq!(running.pause_reason, None);
    assert_eq!(running.token_limit, Some(100));
    assert_eq!(running.cost_limit_usd, Some(0.75));
    assert_eq!(running.accounting_state, GoalAccountingState::Complete);
    assert!(!running.resumable);

    let mut paused = LeaseObservingRegistry::running_for(&ingress, Arc::new(AtomicUsize::new(1)));
    paused.task.status = TaskStatus::Paused;
    paused.goal.pause_reason = Some(GoalPauseReason::OperatorPaused);
    paused.goal.pause_description = Some("paused by operator".to_owned());
    let GoalResponse::Status(paused) =
        status_for_registry(&settings, &ingress, Arc::new(paused)).await
    else {
        panic!("stable paused task should project as status");
    };
    assert_eq!(paused.status, TaskStatus::Paused);
    assert_eq!(paused.execution_epoch, 1);
    assert_eq!(paused.pause_reason, Some(GoalPauseReason::OperatorPaused));
    assert!(paused.resumable);
}

#[tokio::test]
async fn projection_after_terminal_replacement_is_stale_not_corrupt() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let registry = Arc::new(LeaseObservingRegistry::replaced_before_goal_read(&ingress));
    let controller = GoalController::new(registry as Arc<dyn GoalTaskRegistry>);
    let submission = GoalExecutionHost::new()
        .submit(
            &host_settings(&config),
            ingress.clone(),
            recording_driver(&ingress),
            GoalCommand::Status,
        )
        .await
        .unwrap();

    assert!(matches!(
        controller
            .submit(&host_settings(&config), &submission)
            .await
            .unwrap(),
        GoalResponse::Stale
    ));
}

#[tokio::test]
async fn execution_acquisition_releases_the_admission_lease_first() {
    let config = valid_goal_config(true);
    let ingress = matrix_ingress();
    let driver = Arc::new(LeaseDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        available_for_reconnect: Arc::new(AtomicUsize::new(1)),
    });
    let host = GoalExecutionHost::new();
    let submission = host
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
    let scope = GoalExecutionScope::new("goal-1", ingress.session_key().durable_id(), 1).unwrap();
    let checkpoint = GoalParentTurnCheckpoint::new();

    let execution = host
        .acquire_execution(&host_settings(&config), submission, &scope, &checkpoint)
        .await
        .unwrap();
    assert!(
        !driver.reconnect_is_allowed(),
        "the execution lease must replace the released admission lease"
    );
    drop(execution);
    assert!(driver.reconnect_is_allowed());
}

#[tokio::test]
async fn supervisor_rechecks_live_config_at_acquisition_and_settles_refusal() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let ingress = matrix_ingress();
    let session_id = ingress.session_key().durable_id();
    let parent_started = Arc::new(Notify::new());
    let release_parent = Arc::new(Notify::new());
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let pausing = Arc::new(PausingExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        parent_started: Arc::clone(&parent_started),
        release_parent: Arc::clone(&release_parent),
        verifier_calls: Arc::clone(&verifier_calls),
    });
    let driver = Arc::new(LiveConfigObservingDriver {
        inner: pausing,
        store: Arc::clone(&store),
        acquisitions: Mutex::new(Vec::new()),
        calls: Arc::new(OriginalOperationCounts::default()),
        events: Arc::new(Mutex::new(Vec::new())),
        published: Arc::new(Notify::new()),
    });

    let directory = TempDir::new().unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory.path(),
        )
        .unwrap(),
    );
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            Arc::clone(&tracker),
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor =
        GoalExecutionSupervisor::new(Arc::new(runtime.execution_engine(accounting_factory)));
    let snapshot = live.handle().snapshot();
    let mut started = supervisor
        .submit(
            &host_settings(&snapshot),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "refuse acquisition after the verifier is removed".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(started_projection) = started.response() else {
        panic!("valid intake must admit Start");
    };
    let failed_task_id = started_projection.task_id.clone();
    let failed_scope = GoalExecutionScope::new(
        failed_task_id.as_str(),
        session_id.clone(),
        started_projection.execution_epoch,
    )
    .unwrap();
    let mut invalid = live.handle().snapshot();
    invalid.goal.verifier.model_provider = ModelProviderRef::new("openai.missing");
    live.publish(live.next_revision().unwrap(), invalid)
        .unwrap();
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap()
            .is_none()
    );
    timeout(Duration::from_secs(1), driver.published.notified())
        .await
        .expect("the failed task's native error and failure notice must finish publication");
    let failed = timeout(Duration::from_secs(1), async {
        loop {
            let current = store
                .current_goal_for_session(&session_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(current.id, failed_task_id);
            if current.status.is_terminal() {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("refused acquisition must settle the admitted epoch");
    assert_eq!(failed.status, TaskStatus::Failed);
    assert_eq!(
        store
            .terminal_reason_for_session_goal(&failed_task_id, &session_id)
            .await
            .unwrap()
            .as_deref(),
        Some("executor_failed")
    );
    assert_eq!(
        *driver.acquisitions.lock().unwrap(),
        vec![(
            failed_task_id.clone(),
            failed_scope.execution_epoch(),
            TaskStatus::Failed
        )]
    );
    assert_eq!(driver.inner.execution_acquires.load(Ordering::SeqCst), 1);
    assert_eq!(
        *driver.events.lock().unwrap(),
        vec!["original_config_error", "failure_notice"]
    );
    assert_eq!(driver.calls.history.load(Ordering::SeqCst), 0);
    assert_eq!(driver.calls.parent.load(Ordering::SeqCst), 0);
    assert_eq!(driver.calls.append.load(Ordering::SeqCst), 0);
    assert_eq!(driver.calls.verifier.load(Ordering::SeqCst), 0);
    assert_eq!(
        tracker
            .get_summary_for_task(&failed_task_id)
            .unwrap()
            .request_count,
        0
    );
    assert!(
        timeout(Duration::from_millis(20), parent_started.notified())
            .await
            .is_err(),
        "refused acquisition must not start a parent turn"
    );
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
    let goal = store.get_goal_task(&failed_task_id).await.unwrap().unwrap();
    assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    assert!(goal.pending_call_id.is_none());
    assert!(goal.pending_call_epoch.is_none());
    let refusal = timeout(Duration::from_secs(1), supervisor.drain(&failed_scope))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        format!("{refusal:#}"),
        format!("acquire Goal session execution lease: {INVALID_LIVE_VERIFIER_ERROR}")
    );
    assert_eq!(driver.inner.execution_acquires.load(Ordering::SeqCst), 1);
    assert_eq!(
        *driver.events.lock().unwrap(),
        vec!["original_config_error", "failure_notice"]
    );
    drop(started);

    live.publish(live.next_revision().unwrap(), valid_goal_config(true))
        .unwrap();
    let snapshot = live.handle().snapshot();
    let mut restored = supervisor
        .submit(
            &host_settings(&snapshot),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "execute a new goal after restoring valid config".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(restored_projection) = restored.response() else {
        panic!("restored valid config must admit a new Goal");
    };
    assert_ne!(restored_projection.task_id, failed_task_id);
    let restored_task_id = restored_projection.task_id.clone();
    let restored_epoch = restored_projection.execution_epoch;
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut restored)
            .await
            .unwrap()
            .is_none()
    );
    timeout(Duration::from_secs(1), parent_started.notified())
        .await
        .expect("the restored valid config must allow the new parent to start");
    assert_eq!(
        *driver.acquisitions.lock().unwrap(),
        vec![
            (
                failed_task_id.clone(),
                failed_scope.execution_epoch(),
                TaskStatus::Failed
            ),
            (restored_task_id, restored_epoch, TaskStatus::Running),
        ]
    );
    assert_eq!(driver.inner.execution_acquires.load(Ordering::SeqCst), 2);
    assert_eq!(driver.calls.history.load(Ordering::SeqCst), 1);
    assert_eq!(driver.calls.parent.load(Ordering::SeqCst), 1);
    assert_eq!(driver.calls.append.load(Ordering::SeqCst), 0);
    assert_eq!(driver.calls.verifier.load(Ordering::SeqCst), 0);
    assert_eq!(
        *driver.events.lock().unwrap(),
        vec!["original_config_error", "failure_notice"]
    );
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
    let snapshot = live.handle().snapshot();
    let mut cancelled = supervisor
        .submit(
            &host_settings(&snapshot),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        cancelled.response(),
        GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Cancel)
    ));
    release_parent.notify_one();
    assert!(matches!(
        timeout(
            Duration::from_secs(1),
            supervisor.launch_after_response(&settings_source, &mut cancelled),
        )
        .await
        .expect("cleanup must drain the admitted parent after cancellation")
        .unwrap(),
        Some(GoalResponse::Cancelled(_))
    ));
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn verifier_continue_preserves_the_process_local_parent_transcript() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let snapshot = live.handle().snapshot();
    let settings = host_settings(&snapshot);
    let ingress = matrix_ingress();
    let delivered = Arc::new(AtomicUsize::new(0));
    let parent_histories = Arc::new(Mutex::new(Vec::new()));
    let parent_turn_kinds = Arc::new(Mutex::new(Vec::new()));
    let driver = Arc::new(TranscriptExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        delivered: Arc::clone(&delivered),
        parent_histories: Arc::clone(&parent_histories),
        parent_turn_kinds: Arc::clone(&parent_turn_kinds),
    });
    let request = runtime
        .submit(
            &settings,
            ingress,
            driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
        )
        .await
        .unwrap()
        .into_parts()
        .1
        .expect("start must yield an execution request");
    let scope = request.scope().clone();
    let directory = TempDir::new().unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory.path(),
        )
        .unwrap(),
    );
    let engine = runtime.execution_engine(Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    ));

    assert_eq!(
        engine
            .run(&settings_source, request, GoalParentTurnCheckpoint::new())
            .await
            .unwrap(),
        GoalExecutionOutcome::Completed
    );
    assert_eq!(
        delivered.load(Ordering::SeqCst),
        2,
        "each parent candidate is delivered before its verifier decision"
    );
    {
        let histories = parent_histories.lock().unwrap();
        assert_eq!(histories.len(), 2, "Continue must run a second parent turn");
        assert_eq!(
            histories[1]
                .iter()
                .filter(|message| {
                    message.role == "assistant" && message.content == "parent:finish the task"
                })
                .count(),
            1,
            "the prior candidate must occur once despite the Agent transcript containing its final reply"
        );
        assert!(
            histories[1]
                .iter()
                .any(|message| message.content.contains("add the missing detail"))
        );
    }
    assert_eq!(
        parent_turn_kinds.lock().unwrap().as_slice(),
        &[GoalParentTurnKind::Start, GoalParentTurnKind::Continue]
    );
    assert_eq!(
        store
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Completed
    );
}

#[tokio::test]
async fn supervisor_immediate_pause_acknowledges_before_draining_parent() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let ingress = matrix_ingress();
    let parent_started = Arc::new(Notify::new());
    let release_parent = Arc::new(Notify::new());
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(PausingExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        parent_started: Arc::clone(&parent_started),
        release_parent: Arc::clone(&release_parent),
        verifier_calls: Arc::clone(&verifier_calls),
    });
    let directory = TempDir::new().unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory.path(),
        )
        .unwrap(),
    );
    let pricer = Arc::new(ModelProviderGoalUsagePricer::new(Arc::default()));
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            pricer,
        )
        .unwrap(),
    );
    let engine = Arc::new(runtime.execution_engine(accounting_factory));
    let supervisor = Arc::new(GoalExecutionSupervisor::new(engine));

    let mut started = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(started.response(), GoalResponse::Started(_)));
    assert!(
        timeout(Duration::from_millis(20), parent_started.notified())
            .await
            .is_err(),
        "the admitted worker must wait until its surface delivers the start acknowledgement"
    );
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap()
            .is_none()
    );
    parent_started.notified().await;

    let mut pause = timeout(
        Duration::from_millis(20),
        supervisor.submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Pause { immediate: true },
        ),
    )
    .await
    .expect("immediate pause must return before the admitted parent settles")
    .unwrap();
    assert!(matches!(
        pause.response(),
        GoalResponse::TransitionPending(zeroclaw_goal::GoalPendingAction::Pause)
    ));

    let drain_supervisor = Arc::clone(&supervisor);
    let drain_source = settings_source.clone();
    let mut draining = zeroclaw_spawn::spawn!(async move {
        drain_supervisor
            .launch_after_response(&drain_source, &mut pause)
            .await
    });
    assert!(
        timeout(Duration::from_millis(20), &mut draining)
            .await
            .is_err(),
        "post-response drain must wait for the admitted parent operation"
    );

    let mut status = timeout(
        Duration::from_millis(20),
        supervisor.submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Status,
        ),
    )
    .await
    .expect("draining a fenced parent must not hold the Goal command lease")
    .unwrap();
    assert!(matches!(status.response(), GoalResponse::Status(_)));
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut status)
            .await
            .unwrap()
            .is_none()
    );

    release_parent.notify_one();
    assert!(matches!(
        draining.await.unwrap().unwrap(),
        Some(GoalResponse::Paused(_))
    ));
    assert_eq!(
        verifier_calls.load(Ordering::SeqCst),
        0,
        "a pause may drain an admitted parent operation but cannot admit a verifier"
    );
}

#[tokio::test]
async fn verifier_block_publishes_the_parent_request_before_goal_pause() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let ingress = matrix_ingress();
    let events = Arc::new(Mutex::new(Vec::new()));
    let candidates = Arc::new(Mutex::new(Vec::new()));
    let driver = Arc::new(UserInputExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        events: Arc::clone(&events),
        candidates: Arc::clone(&candidates),
    });
    let directory = TempDir::new().unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory.path(),
        )
        .unwrap(),
    );
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let engine = Arc::new(runtime.execution_engine(accounting_factory));
    let supervisor = GoalExecutionSupervisor::new(engine);

    let mut started = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "choose the appropriate option".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(started.response(), GoalResponse::Started(_)));
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap()
            .is_none()
    );

    let paused = timeout(Duration::from_secs(1), async {
        loop {
            let current = store
                .current_goal_for_session(&ingress.session_key().durable_id())
                .await
                .unwrap()
                .unwrap();
            if current.status == TaskStatus::Paused {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a validated user-input blocker must pause the Goal");

    assert_eq!(paused.status, TaskStatus::Paused);
    assert_eq!(
        candidates.lock().unwrap().as_slice(),
        &["Which option should I use?".to_owned()]
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        &["parent", "candidate", "verifier", "paused"]
    );
}

#[tokio::test]
async fn supervisor_pauses_without_starting_when_the_surface_cannot_acknowledge() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let ingress = matrix_ingress();
    let parent_started = Arc::new(Notify::new());
    let release_parent = Arc::new(Notify::new());
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(PausingExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        parent_started: Arc::clone(&parent_started),
        release_parent,
        verifier_calls: Arc::clone(&verifier_calls),
    });
    let directory = TempDir::new().unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory.path(),
        )
        .unwrap(),
    );
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor =
        GoalExecutionSupervisor::new(Arc::new(runtime.execution_engine(accounting_factory)));

    let mut started = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver,
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
        )
        .await
        .unwrap();
    let task_id = match started.response() {
        GoalResponse::Started(projection) => projection.task_id.clone(),
        response => panic!("expected start response, got {response:?}"),
    };
    supervisor
        .pause_before_launch(&mut started, GoalPauseReason::ExternalDependency)
        .await
        .unwrap();

    assert!(
        timeout(Duration::from_millis(20), parent_started.notified())
            .await
            .is_err(),
        "a Goal whose lifecycle acknowledgement was not delivered must not start"
    );
    let task = store
        .current_goal_for_session(&ingress.session_key().durable_id())
        .await
        .unwrap()
        .unwrap();
    let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Paused);
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::ExternalDependency));
    assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    assert!(goal.pending_call_id.is_none());
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
}

async fn assert_launch_rebind_failure_outcome(cancel_before_launch: bool) {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
        let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let live = LiveConfig::new(valid_goal_config(true));
        let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
        let directory = TempDir::new().unwrap();
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: false,
                    ..Default::default()
                },
                directory.path(),
            )
            .unwrap(),
        );
        let accounting_factory = Arc::new(
            RuntimeGoalOperationAccountingFactory::new(
                store.clone() as Arc<dyn GoalTaskRegistry>,
                tracker,
                "main",
                Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
            )
            .unwrap(),
        );
        let supervisor =
            GoalExecutionSupervisor::new(Arc::new(runtime.execution_engine(accounting_factory)));
        let inner = recording_driver(&ingress);
        let driver = Arc::new(LaunchRebindFailureDriver {
            inner: Arc::clone(&inner),
            refuse_bind: AtomicBool::new(false),
            notices: Arc::new(Mutex::new(Vec::new())),
            parent_calls: Arc::new(AtomicUsize::new(0)),
        });
        let mut started = supervisor
            .submit(
                &host_settings(&live.handle().snapshot()),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "finish the admitted task".into(),
                },
            )
            .await
            .unwrap();
        let task_id = match started.response() {
            GoalResponse::Started(projection) => projection.task_id.clone(),
            response => panic!("expected start response, got {response:?}"),
        };
        if cancel_before_launch {
            let cancelled = supervisor
                .submit(
                    &host_settings(&live.handle().snapshot()),
                    ingress.clone(),
                    recording_driver(&ingress),
                    GoalCommand::Cancel,
                )
                .await
                .unwrap();
            assert!(matches!(cancelled.response(), GoalResponse::Cancelled(_)));
        }
        driver.refuse_bind.store(true, Ordering::SeqCst);
        let error = supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("session binding revoked after acknowledgement"),
            "the original bind error must be retained: {error:#}"
        );
        let session_id = ingress.session_key().durable_id();
        let current = store
            .current_goal_for_session(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.id, task_id);
        let expected_status = if cancel_before_launch {
            TaskStatus::Cancelled
        } else {
            TaskStatus::Failed
        };
        assert_eq!(current.status, expected_status);
        let reason = store
            .terminal_reason_for_session_goal(&task_id, &session_id)
            .await
            .unwrap();
        if !cancel_before_launch {
            assert_eq!(reason.as_deref(), Some("executor_start_failed"));
        } else {
            assert_ne!(reason.as_deref(), Some("executor_start_failed"));
        }
        let expected_notices = if cancel_before_launch {
            Vec::new()
        } else {
            vec![zeroclaw_goal::GoalExecutionNotice::Failed(
                zeroclaw_goal::GoalFailureReason::ExecutorStartFailed,
            )]
        };
        assert_eq!(*driver.notices.lock().unwrap(), expected_notices);
        assert_eq!(
            inner.execution_acquires.load(Ordering::SeqCst),
            usize::from(!cancel_before_launch),
            "only failure-notice delivery may acquire a lease"
        );
        assert_eq!(driver.parent_calls.load(Ordering::SeqCst), 0);
        assert!(
            supervisor
                .launch_after_response(&settings_source, &mut started)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(*driver.notices.lock().unwrap(), expected_notices);
    }
}

#[tokio::test]
async fn launch_rebind_failure_settles_the_admitted_goal_before_returning() {
    assert_launch_rebind_failure_outcome(false).await;
}

async fn assert_post_commit_projection_failure_is_settled(resume: bool, cancelled: bool) {
    for ingress in [matrix_ingress(), zerocode_ingress("main")] {
        let directory = TempDir::new().unwrap();
        let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
        let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
        let live = LiveConfig::new(valid_goal_config(true));
        let session_id = ingress.session_key().durable_id();
        let driver = recording_driver(&ingress);
        let start = GoalCommand::Start {
            budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
            objective: "finish the bounded task".into(),
        };
        if resume {
            let initial = runtime
                .submit(
                    &host_settings(&live.handle().snapshot()),
                    ingress.clone(),
                    driver.clone(),
                    start.clone(),
                )
                .await
                .unwrap();
            let GoalResponse::Started(projection) = initial.response() else {
                panic!("expected initial start");
            };
            assert_eq!(
                store
                    .pause_session_goal(
                        &projection.task_id,
                        &session_id,
                        projection.execution_epoch,
                        GoalPauseState {
                            reason: GoalPauseReason::OperatorPaused,
                            description: Some("operator pause".into()),
                            blockers: Vec::new(),
                        },
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
        }
        let connection =
            rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
        // Corrupt only the projection payload after the real transition.
        // Neither injection substitutes the registry or introduces an await.
        let transition = if cancelled {
            "UPDATE tasks SET status = 'cancelled', execution_epoch = execution_epoch + 1,
             finished_at = 'fixture-finished', error = 'fixture-cancelled'
             WHERE id = NEW.task_id;"
        } else {
            ""
        };
        let trigger = if resume {
            format!(
                "CREATE TRIGGER fail_projection AFTER UPDATE OF pause_reason ON goal_tasks
             WHEN NEW.pause_reason IS NULL
             BEGIN UPDATE goal_tasks SET blockers_json = 'invalid-json'
                   WHERE task_id = NEW.task_id; {transition} END;"
            )
        } else {
            format!(
                "CREATE TRIGGER fail_projection AFTER INSERT ON goal_tasks
             BEGIN UPDATE goal_tasks SET blockers_json = 'invalid-json'
                   WHERE task_id = NEW.task_id; {transition} END;"
            )
        };
        connection.execute_batch(&trigger).unwrap();
        let command = if resume {
            GoalCommand::Resume { response: None }
        } else {
            start
        };
        let snapshot = live.handle().snapshot();
        let settings = host_settings(&snapshot);
        let mut submission =
            Box::pin(runtime.submit(&settings, ingress.clone(), driver.clone(), command));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(result) =
            std::future::Future::poll(submission.as_mut(), &mut context)
        else {
            panic!("SQLite admission and projection must settle within one poll");
        };
        let error = result
            .err()
            .expect("post-commit projection must report its original read error");
        assert!(format!("{error:#}").contains("get goal task"));
        let current = store
            .current_goal_for_session(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            current.status,
            if cancelled {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Failed
            },
            "post-commit projection error must not leave an unowned running epoch"
        );
        assert_eq!(current.execution_epoch, if resume { 4 } else { 2 });
        assert_eq!(
            store
                .terminal_reason_for_session_goal(&current.id, &session_id)
                .await
                .unwrap()
                .as_deref(),
            Some(if cancelled {
                "fixture-cancelled"
            } else {
                "executor_start_failed"
            })
        );
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn start_projection_failure_settles_the_committed_epoch() {
    assert_post_commit_projection_failure_is_settled(false, false).await;
}

fn unlaunched_sqlite_supervisor(
    store: Arc<SqliteTaskStore>,
    directory: &std::path::Path,
) -> (GoalExecutionSupervisor, Arc<CostTracker>) {
    let registry = store as Arc<dyn GoalTaskRegistry>;
    let runtime = GoalRuntime::new(Arc::clone(&registry));
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory,
        )
        .unwrap(),
    );
    let factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            registry,
            Arc::clone(&tracker),
            "main",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    (
        GoalExecutionSupervisor::new(Arc::new(runtime.execution_engine(factory))),
        tracker,
    )
}

#[tokio::test]
async fn unlaunched_pause_write_failure_retains_ownership_until_explicit_retry() {
    let directory = TempDir::new().unwrap();
    let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
    let (supervisor, tracker) = unlaunched_sqlite_supervisor(Arc::clone(&store), directory.path());
    let ingress = matrix_ingress();
    let driver = recording_driver(&ingress);
    let config = valid_goal_config(true);
    let submission = supervisor
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the bounded fixture task".into(),
            },
        )
        .await
        .unwrap();
    let GoalResponse::Started(projection) = submission.response() else {
        panic!("expected an admitted fixture epoch");
    };
    let scope = GoalExecutionScope::new(
        &projection.task_id,
        ingress.session_key().durable_id(),
        projection.execution_epoch,
    )
    .unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_unlaunched_pause BEFORE UPDATE OF status ON tasks
             WHEN NEW.status = 'paused'
             BEGIN SELECT RAISE(ABORT, 'fixture unlaunched pause write failure'); END;",
        )
        .unwrap();
    drop(submission);

    let error = supervisor.pause_for_restart().await.unwrap_err();
    assert!(format!("{error:#}").contains("fixture unlaunched pause write failure"));
    let current = store
        .current_goal_for_session(scope.session_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.status, TaskStatus::Running);
    assert_eq!(current.execution_epoch, scope.execution_epoch());
    assert!(supervisor.owns_scope(&scope).await);
    assert!(!supervisor.try_retire().await);
    let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
    assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    assert!(goal.pending_call_id.is_none());
    assert!(goal.pending_call_epoch.is_none());
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);

    connection
        .execute_batch("DROP TRIGGER reject_unlaunched_pause;")
        .unwrap();
    let retained_error = supervisor.pause_for_restart().await.unwrap_err();
    assert!(format!("{retained_error:#}").contains("fixture unlaunched pause write failure"));
    let current = store
        .current_goal_for_session(scope.session_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.status, TaskStatus::Paused);
    assert_eq!(current.execution_epoch, scope.execution_epoch() + 1);
    assert_eq!(
        store
            .get_goal_task(scope.task_id())
            .await
            .unwrap()
            .unwrap()
            .pause_reason,
        Some(GoalPauseReason::DaemonRestart)
    );
    assert!(!supervisor.owns_scope(&scope).await);
    assert!(supervisor.try_retire().await);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    assert_eq!(
        tracker.get_usage_totals_for_task(scope.task_id()).unwrap(),
        (0, 0.0)
    );
}

#[tokio::test]
async fn supervised_projection_and_finish_failure_keep_the_committed_epoch_owned() {
    for resume in [false, true] {
        let directory = TempDir::new().unwrap();
        let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
        let (supervisor, tracker) =
            unlaunched_sqlite_supervisor(Arc::clone(&store), directory.path());
        let ingress = matrix_ingress();
        let session_id = ingress.session_key().durable_id();
        let driver = recording_driver(&ingress);
        let config = valid_goal_config(true);
        let start = GoalCommand::Start {
            budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
            objective: "finish the supervised fixture task".into(),
        };
        if resume {
            let mut initial = supervisor
                .submit(
                    &host_settings(&config),
                    ingress.clone(),
                    driver.clone(),
                    start.clone(),
                )
                .await
                .unwrap();
            supervisor
                .pause_before_launch(&mut initial, GoalPauseReason::OperatorPaused)
                .await
                .unwrap();
            drop(initial);
        }
        let connection =
            rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_projection_finish BEFORE UPDATE OF status ON tasks
             WHEN NEW.status = 'failed'
             BEGIN SELECT RAISE(ABORT, 'fixture projection cleanup write failure'); END;",
            )
            .unwrap();
        connection.execute_batch(if resume {
            "CREATE TRIGGER corrupt_owned_projection AFTER UPDATE OF pause_reason ON goal_tasks
             WHEN NEW.pause_reason IS NULL
             BEGIN UPDATE goal_tasks SET blockers_json = 'invalid-json' WHERE task_id = NEW.task_id; END;"
        } else {
            "CREATE TRIGGER corrupt_owned_projection AFTER INSERT ON goal_tasks
             BEGIN UPDATE goal_tasks SET blockers_json = 'invalid-json' WHERE task_id = NEW.task_id; END;"
        }).unwrap();
        let command = if resume {
            GoalCommand::Resume { response: None }
        } else {
            start
        };
        let error = supervisor
            .submit(
                &host_settings(&config),
                ingress.clone(),
                driver.clone(),
                command,
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("get goal task"), "{error:#}");
        assert!(
            format!("{error:#}").contains("fixture projection cleanup write failure"),
            "{error:#}"
        );
        assert!(
            error.chain().any(|cause| matches!(
                cause.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::FromSqlConversionFailure(..))
            )),
            "original projection type must survive: {error:#}"
        );
        let current = store
            .current_goal_for_session(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.status, TaskStatus::Running);
        assert_eq!(current.execution_epoch, if resume { 3 } else { 1 });
        let scope =
            GoalExecutionScope::new(&current.id, &session_id, current.execution_epoch).unwrap();
        assert!(supervisor.owns_scope(&scope).await);
        assert!(!supervisor.try_retire().await);
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);

        connection
            .execute_batch(
                "DROP TRIGGER corrupt_owned_projection;
             DROP TRIGGER reject_projection_finish;
             UPDATE goal_tasks SET blockers_json = '[]';",
            )
            .unwrap();
        let retained_error = supervisor.pause_for_restart().await.unwrap_err();
        assert!(
            format!("{retained_error:#}").contains("get goal task"),
            "{retained_error:#}"
        );
        assert!(
            retained_error.chain().any(|cause| matches!(
                cause.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::FromSqlConversionFailure(..))
            )),
            "retained projection type must survive: {retained_error:#}"
        );
        let current = store
            .current_goal_for_session(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.status, TaskStatus::Failed);
        assert_eq!(current.execution_epoch, scope.execution_epoch() + 1);
        assert_eq!(
            store
                .terminal_reason_for_session_goal(&current.id, &session_id)
                .await
                .unwrap()
                .as_deref(),
            Some("executor_start_failed")
        );
        let goal = store.get_goal_task(&current.id).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert!(!supervisor.owns_scope(&scope).await);
        assert!(supervisor.try_retire().await);
        assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
        assert_eq!(
            tracker.get_usage_totals_for_task(scope.task_id()).unwrap(),
            (0, 0.0)
        );
    }
}

#[tokio::test]
async fn unlaunched_saturated_epoch_finishes_without_overflow_or_model_work() {
    let directory = TempDir::new().unwrap();
    let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
    let (supervisor, tracker) = unlaunched_sqlite_supervisor(Arc::clone(&store), directory.path());
    let ingress = matrix_ingress();
    let session_id = ingress.session_key().durable_id();
    let driver = recording_driver(&ingress);
    let config = valid_goal_config(true);
    let mut initial = supervisor
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the saturated fixture task".into(),
            },
        )
        .await
        .unwrap();
    supervisor
        .pause_before_launch(&mut initial, GoalPauseReason::OperatorPaused)
        .await
        .unwrap();
    drop(initial);
    let connection = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
    connection
        .execute(
            "UPDATE tasks SET execution_epoch = ?1 WHERE session_id = ?2",
            rusqlite::params![i64::MAX - 1, &session_id],
        )
        .unwrap();
    let resumed = supervisor
        .submit(
            &host_settings(&config),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    let GoalResponse::Resumed(projection) = resumed.response() else {
        panic!("expected saturated admitted epoch");
    };
    assert_eq!(projection.execution_epoch, i64::MAX);
    let scope = GoalExecutionScope::new(&projection.task_id, &session_id, i64::MAX).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_saturated_cleanup BEFORE UPDATE OF status ON tasks
         WHEN OLD.execution_epoch = 9223372036854775807 AND NEW.status = 'failed'
         BEGIN SELECT RAISE(ABORT, 'fixture saturated cleanup write failure'); END;",
        )
        .unwrap();
    drop(resumed);
    let write_error = supervisor.pause_for_restart().await.unwrap_err();
    assert!(
        format!("{write_error:#}").contains("fixture saturated cleanup write failure"),
        "{write_error:#}"
    );
    let pending = store
        .current_goal_for_session(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.status, TaskStatus::Running);
    assert_eq!(pending.execution_epoch, i64::MAX);
    assert!(supervisor.owns_scope(&scope).await);
    assert!(!supervisor.try_retire().await);
    connection
        .execute_batch("DROP TRIGGER reject_saturated_cleanup;")
        .unwrap();
    let retained_error = supervisor.pause_for_restart().await.unwrap_err();
    assert!(
        format!("{retained_error:#}").contains("fixture saturated cleanup write failure"),
        "{retained_error:#}"
    );
    let current = store
        .current_goal_for_session(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.id, scope.task_id());
    assert_eq!(current.status, TaskStatus::Failed);
    assert_eq!(current.execution_epoch, i64::MAX);
    assert_eq!(
        store
            .terminal_reason_for_session_goal(&current.id, &session_id)
            .await
            .unwrap()
            .as_deref(),
        Some("executor_start_failed")
    );
    assert!(!supervisor.owns_scope(&scope).await);
    assert!(supervisor.try_retire().await);
    assert_eq!(driver.execution_acquires.load(Ordering::SeqCst), 0);
    assert_eq!(
        tracker.get_usage_totals_for_task(scope.task_id()).unwrap(),
        (0, 0.0)
    );
}

#[tokio::test]
async fn resume_projection_failure_settles_the_committed_epoch() {
    assert_post_commit_projection_failure_is_settled(true, false).await;
}

#[tokio::test]
async fn projection_failure_preserves_a_competing_cancellation() {
    assert_post_commit_projection_failure_is_settled(false, true).await;
    assert_post_commit_projection_failure_is_settled(true, true).await;
}

#[tokio::test]
async fn launch_rebind_failure_preserves_a_competing_cancellation() {
    assert_launch_rebind_failure_outcome(true).await;
}

#[tokio::test]
async fn daemon_restart_fences_goal_until_explicit_resume_in_the_next_generation() {
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone() as Arc<dyn GoalTaskRegistry>);
    let live = LiveConfig::new(valid_goal_config(true));
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "test-boot").unwrap();
    let ingress = matrix_ingress();
    let parent_started = Arc::new(Notify::new());
    let release_parent = Arc::new(Notify::new());
    let verifier_calls = Arc::new(AtomicUsize::new(0));
    let driver = Arc::new(PausingExecutionDriver {
        binding: GoalSessionBinding::new(ingress.session_key().clone()),
        execution_acquires: AtomicUsize::new(0),
        parent_started: Arc::clone(&parent_started),
        release_parent: Arc::clone(&release_parent),
        verifier_calls: Arc::clone(&verifier_calls),
    });
    let directory = TempDir::new().unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory.path(),
        )
        .unwrap(),
    );
    let pricer = Arc::new(ModelProviderGoalUsagePricer::new(Arc::default()));
    let accounting_factory = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone() as Arc<dyn GoalTaskRegistry>,
            tracker,
            "main",
            pricer,
        )
        .unwrap(),
    );
    let engine = Arc::new(runtime.execution_engine(accounting_factory));
    let coordinator = Arc::new(GoalExecutionRestartCoordinator::new());
    let supervisor = coordinator.new_supervisor(engine).await;

    let mut started = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            },
        )
        .await
        .unwrap();
    let task_id = match started.response() {
        GoalResponse::Started(projection) => projection.task_id.clone(),
        response => panic!("expected start response, got {response:?}"),
    };
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut started)
            .await
            .unwrap()
            .is_none()
    );
    parent_started.notified().await;

    let restarting = Arc::clone(&coordinator);
    let mut quiesce = zeroclaw_spawn::spawn!(async move { restarting.quiesce_for_restart().await });
    let paused_task = timeout(Duration::from_secs(1), async {
        loop {
            let current = store
                .current_goal_for_session(&ingress.session_key().durable_id())
                .await
                .unwrap()
                .unwrap();
            if current.status == TaskStatus::Paused {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("restart must durably fence the active Goal");
    assert_eq!(paused_task.id, task_id);
    let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::DaemonRestart));
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
    assert!(
        timeout(Duration::from_millis(20), &mut quiesce)
            .await
            .is_err(),
        "restart must drain the already-admitted parent operation"
    );

    release_parent.notify_one();
    quiesce.await.unwrap().unwrap();
    assert_eq!(
        store
            .current_goal_for_session(&ingress.session_key().durable_id())
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Paused,
        "restart quiescence must not auto-resume the Goal"
    );

    let rejected_resume = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Resume { response: None },
        )
        .await;
    assert!(
        rejected_resume.is_err(),
        "the retiring generation must not admit a Goal resume"
    );

    coordinator.reopen_for_generation().await;
    let mut resumed = supervisor
        .submit(
            &host_settings(&live.handle().snapshot()),
            ingress.clone(),
            driver,
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
    assert!(matches!(resumed.response(), GoalResponse::Resumed(_)));
    assert!(
        supervisor
            .launch_after_response(&settings_source, &mut resumed)
            .await
            .unwrap()
            .is_none()
    );
    parent_started.notified().await;
    release_parent.notify_one();
    let completed = timeout(Duration::from_secs(1), async {
        loop {
            let current = store
                .current_goal_for_session(&ingress.session_key().durable_id())
                .await
                .unwrap()
                .unwrap();
            if current.status.is_terminal() {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("explicit resume must execute in the next generation");
    assert_eq!(
        completed.status,
        TaskStatus::Completed,
        "resumed Goal terminal record: {completed:?}"
    );
    assert_eq!(verifier_calls.load(Ordering::SeqCst), 1);
}
