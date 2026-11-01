use std::sync::{
    Arc, Mutex, Once,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use tokio::{
    sync::Notify,
    time::{Duration, timeout},
};
use zeroclaw_api::{model_provider::ChatMessage, model_provider::TokenUsage, task::TaskStatus};
use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
use zeroclaw_config::{
    cost::CostTracker,
    goal::{GoalConfig, GoalVerifierConfig},
    live::LiveConfig,
    providers::ModelProviderRef,
    schema::{Config, CostConfig, ModelProviderConfig, OpenAIModelProviderConfig},
};
use zeroclaw_goal::{
    GoalAccountingState, GoalExecutionNotice, GoalExecutionOutcome, GoalExecutionScope,
    GoalExecutionSupervisor, GoalFailureReason, GoalHostSettings, GoalHostSettingsSource,
    GoalIngressContext, GoalIngressPrincipal, GoalOperationScope, GoalParentTurn,
    GoalParentTurnCheckpoint, GoalParentTurnKind, GoalParentTurnResult, GoalPauseReason,
    GoalPauseState, GoalResponse, GoalRuntime, GoalSessionBinding, GoalSessionDriver,
    GoalSessionExecutionLease, GoalSessionKey, GoalSessionLease, GoalTaskRegistry,
    GoalTransitionResult, GoalVerifierResult, GoalVerifierTurn,
    accounting::{
        GOAL_OPERATION_ACCOUNTING, GoalAccountingFailure, GoalOperationFenced,
        GoalOperationRequest, GoalOperationSettlement, GoalUsageEvent,
    },
};
use zeroclaw_runtime::{
    control_plane::SqliteTaskStore,
    goal_accounting::{ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Window {
    Parent,
    Verifier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Pause,
    Resume,
    Restart,
    ExternalCancellation,
    Cancel,
    StillRunning,
    Budget,
    StillRunningBudget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Fenced,
    Plain,
    Pending,
    Missing,
}

fn accounting_failure_chain(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::Pending => {
            "Goal failure lost its execution fence: Goal accounting is incomplete: an earlier Goal model call has not settled; no further Goal model call will be admitted"
        }
        Refusal::Missing => {
            "Goal failure lost its execution fence: Goal accounting is incomplete: usage was not reported for an earlier Goal model call; no further Goal model call will be admitted"
        }
        _ => panic!("only incomplete accounting owns a native core error in this fixture"),
    }
}

struct Observation {
    ready: Notify,
    release: Notify,
    gates: AtomicUsize,
    admissions: AtomicUsize,
    settlements: AtomicUsize,
    budget_fences: AtomicUsize,
    parents: AtomicUsize,
    verifiers: AtomicUsize,
    events: Mutex<Vec<String>>,
}

struct Driver {
    key: GoalSessionKey,
    store: Arc<SqliteTaskStore>,
    observation: Arc<Observation>,
    window: Window,
    control: Control,
    refusal: Refusal,
}

struct Lease {
    key: GoalSessionKey,
    store: Arc<SqliteTaskStore>,
    observation: Arc<Observation>,
    window: Window,
    control: Control,
    refusal: Refusal,
}

#[async_trait]
impl GoalSessionDriver for Driver {
    fn session_key(&self) -> &GoalSessionKey {
        &self.key
    }

    async fn bind(&self, _: &GoalIngressContext) -> anyhow::Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(
            GoalSessionBinding::new(self.key.clone()),
            (),
        ))
    }

    async fn acquire_execution(
        &self,
        _: &GoalIngressContext,
        _: &GoalExecutionScope,
        _: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        Ok(Box::new(Lease {
            key: self.key.clone(),
            store: self.store.clone(),
            observation: self.observation.clone(),
            window: self.window,
            control: self.control,
            refusal: self.refusal,
        }))
    }
}

impl Lease {
    async fn settle_operation(&self) -> anyhow::Result<()> {
        let accountant = GOAL_OPERATION_ACCOUNTING.with(Clone::clone).unwrap();
        accountant
            .admit(GoalOperationRequest::new("fixture.route", "fixture-model"))
            .await?;
        self.observation.admissions.fetch_add(1, Ordering::SeqCst);
        accountant
            .settle(GoalOperationSettlement {
                issues: Vec::new(),
                accounting_state: GoalAccountingState::Complete,
                events: vec![GoalUsageEvent {
                    provider_ref: "fixture.route".into(),
                    model: "fixture-model".into(),
                    usage: TokenUsage {
                        input_tokens: Some(10),
                        output_tokens: Some(2),
                        cached_input_tokens: Some(0),
                        cache_creation_input_tokens: Some(0),
                    },
                    priceable: true,
                }],
            })
            .await?;
        self.observation.settlements.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn refuse_after_control(&self, scope: &GoalOperationScope) -> anyhow::Result<()> {
        let execution = scope.execution();
        if matches!(self.refusal, Refusal::Pending | Refusal::Missing) {
            assert_eq!(
                self.store
                    .admit_pending_operation(
                        execution.task_id(),
                        execution.session_id(),
                        execution.execution_epoch(),
                        "fixture-uncertain-operation",
                    )
                    .await?,
                GoalTransitionResult::Applied
            );
            if self.refusal == Refusal::Missing {
                assert_eq!(
                    self.store
                        .settle_pending_operation(
                            execution.task_id(),
                            execution.session_id(),
                            execution.execution_epoch(),
                            "fixture-uncertain-operation",
                            GoalAccountingState::Missing,
                        )
                        .await?,
                    GoalTransitionResult::Applied
                );
            }
        }
        self.observation.gates.fetch_add(1, Ordering::SeqCst);
        self.observation.ready.notify_one();
        self.observation.release.notified().await;
        let error = if self.control == Control::StillRunning {
            // The public marker alone is not proof of a committed fence.
            anyhow::Error::new(GoalOperationFenced::NOT_RUNNING)
        } else if self.control == Control::StillRunningBudget {
            // A forged budget marker cannot substitute for a durable pause.
            anyhow::Error::new(GoalOperationFenced::BUDGET_EXHAUSTED)
        } else if self.refusal == Refusal::Plain {
            anyhow::Error::msg("fixture untyped error after pause")
        } else {
            GOAL_OPERATION_ACCOUNTING
                .with(Clone::clone)
                .unwrap()
                .admit(GoalOperationRequest::new("fixture.route", "fixture-model"))
                .await
                .expect_err("the committed transition must refuse admission")
        };
        if self.control == Control::Budget {
            // Observe the causal type without panicking before the actual
            // engine consumer and supervisor drain receive this error.
            if error.downcast_ref::<GoalOperationFenced>().is_some() {
                self.observation
                    .budget_fences
                    .fetch_add(1, Ordering::SeqCst);
            }
        }
        self.observation
            .events
            .lock()
            .unwrap()
            .push(format!("host_error:{error}"));
        // Preserve the actual error type through context, as ordinary hosts do.
        Err(error.context("fixture host operation"))
    }
}

#[async_trait]
impl GoalSessionExecutionLease for Lease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.key
    }

    fn canonical_history(&self) -> anyhow::Result<Vec<ChatMessage>> {
        Ok(vec![])
    }

    async fn run_parent_turn(
        &mut self,
        scope: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        self.observation.parents.fetch_add(1, Ordering::SeqCst);
        self.settle_operation().await?;
        if self.window == Window::Parent && turn.kind != GoalParentTurnKind::Resume {
            self.refuse_after_control(scope).await?;
        }
        GoalParentTurnResult::from_completed_agent_turn(
            "bounded candidate".into(),
            vec![ChatMessage::assistant("bounded candidate")],
        )
    }

    async fn run_verifier(
        &mut self,
        scope: &GoalOperationScope,
        _: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        self.observation.verifiers.fetch_add(1, Ordering::SeqCst);
        self.settle_operation().await?;
        if self.window == Window::Verifier {
            self.refuse_after_control(scope).await?;
            unreachable!("a refused verifier cannot produce a decision");
        }
        Ok(GoalVerifierResult {
            response: r#"{"decision":"complete","reason":"bounded work complete","blockers":[]}"#
                .into(),
            attribution: None,
        })
    }

    async fn append_parent_candidate(&mut self, _: String) -> anyhow::Result<()> {
        Ok(())
    }

    async fn publish_goal_core_error(&mut self, error: &anyhow::Error) -> anyhow::Result<()> {
        let failure = error.downcast_ref::<GoalAccountingFailure>().unwrap();
        let pending = self.refusal == Refusal::Pending;
        assert_eq!(
            failure.accounting_state(),
            if pending {
                GoalAccountingState::Complete
            } else {
                assert_eq!(self.refusal, Refusal::Missing);
                GoalAccountingState::Missing
            }
        );
        assert_eq!(failure.has_pending_operation(), pending);
        assert!(error.downcast_ref::<GoalOperationFenced>().is_none());
        assert_eq!(format!("{error:#}"), accounting_failure_chain(self.refusal));

        // The error retains its creation-time metadata. Drain owns the later
        // pending settlement and terminal transition before native publication.
        let task = self
            .store
            .current_goal_for_session(&self.key.durable_id())
            .await?
            .unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        let goal = self.store.get_goal_task(&task.id).await?.unwrap();
        assert_eq!(
            goal.accounting_state,
            if pending {
                GoalAccountingState::OutcomeUnknown
            } else {
                GoalAccountingState::Missing
            }
        );
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        self.observation
            .events
            .lock()
            .unwrap()
            .push(format!("core_error:{error:#}"));
        Ok(())
    }

    async fn publish_goal_notice(&mut self, notice: GoalExecutionNotice) -> anyhow::Result<()> {
        self.observation
            .events
            .lock()
            .unwrap()
            .push(format!("notice:{notice:?}"));
        Ok(())
    }
}

async fn run_case(key: GoalSessionKey, window: Window, control: Control, refusal: Refusal) {
    static LOG_INIT: Once = Once::new();
    LOG_INIT.call_once(|| {
        zeroclaw_log::try_install_capture_subscriber();
        drop(zeroclaw_log::subscribe_or_install());
    });
    let mut logs = zeroclaw_log::subscribe().unwrap();
    let store = Arc::new(SqliteTaskStore::new_in_memory().unwrap());
    let runtime = GoalRuntime::new(store.clone());
    let directory = tempfile::tempdir().unwrap();
    let tracker = Arc::new(CostTracker::new(CostConfig::default(), directory.path()).unwrap());
    let accountant = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone(),
            tracker.clone(),
            "fixture-agent",
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
        runtime.execution_engine(accountant),
    )));
    let observation = Arc::new(Observation {
        ready: Notify::new(),
        release: Notify::new(),
        gates: AtomicUsize::new(0),
        admissions: AtomicUsize::new(0),
        settlements: AtomicUsize::new(0),
        budget_fences: AtomicUsize::new(0),
        parents: AtomicUsize::new(0),
        verifiers: AtomicUsize::new(0),
        events: Mutex::new(vec![]),
    });
    let driver = Arc::new(Driver {
        key: key.clone(),
        store: store.clone(),
        observation: observation.clone(),
        window,
        control,
        refusal,
    });
    let ingress = GoalIngressContext::trusted(
        key.clone(),
        "fixture-agent",
        "fixture-origin",
        match &key {
            GoalSessionKey::Matrix { .. } => GoalIngressPrincipal::Matrix {
                raw_mxid: "@fixture:example.invalid".into(),
            },
            GoalSessionKey::ZeroCode { .. } => GoalIngressPrincipal::ZeroCode {
                tui_id: "fixture-owner".into(),
            },
        },
    )
    .unwrap();
    let mut config = Config {
        goal: GoalConfig {
            enabled: true,
            default_token_limit: Some(100),
            default_cost_limit_usd: Some(0.0),
            include_channel_turn_context: true,
            verifier: GoalVerifierConfig {
                model_provider: ModelProviderRef::new("openai.default"),
                model: None,
            },
        },
        ..Default::default()
    };
    config.providers.models.openai.insert(
        "default".to_owned(),
        OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("gpt-test".to_owned()),
                ..Default::default()
            },
        },
    );
    let live = LiveConfig::new(config);
    let settings_source = GoalHostSettingsSource::new(live.handle(), 42, "fixture-boot").unwrap();
    let command_config = live.handle().snapshot();
    let mut start = supervisor
        .submit(
            &GoalHostSettings::from_config(&command_config, 42, "fixture-boot").unwrap(),
            ingress.clone(),
            driver.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "finish bounded work".into(),
            },
        )
        .await
        .unwrap();
    let original = store
        .current_goal_for_session(&key.durable_id())
        .await
        .unwrap()
        .unwrap();
    let scope =
        GoalExecutionScope::new(&original.id, key.durable_id(), original.execution_epoch).unwrap();
    zeroclaw_log::record!(
        INFO,
        zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
            .with_attrs(serde_json::json!({"task_id": original.id, "fixture_probe": true})),
        "Fence-refusal fixture capture probe"
    );
    supervisor
        .launch_after_response(&settings_source, &mut start)
        .await
        .unwrap();
    timeout(Duration::from_secs(5), observation.ready.notified())
        .await
        .unwrap();
    let late = match control {
        Control::Pause | Control::Resume | Control::Cancel => {
            let command_config = live.handle().snapshot();
            let mut submission = supervisor
                .submit(
                    &GoalHostSettings::from_config(&command_config, 42, "fixture-boot").unwrap(),
                    ingress.clone(),
                    driver.clone(),
                    if control == Control::Cancel {
                        GoalCommand::Cancel
                    } else {
                        GoalCommand::Pause { immediate: true }
                    },
                )
                .await
                .unwrap();
            let fenced = store
                .current_goal_for_session(&key.durable_id())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(fenced.execution_epoch, original.execution_epoch + 1);
            observation.release.notify_one();
            timeout(
                Duration::from_secs(5),
                supervisor.launch_after_response(&settings_source, &mut submission),
            )
            .await
            .unwrap()
            .unwrap()
        }
        Control::Restart => {
            let owned = supervisor.clone();
            let worker = zeroclaw_spawn::spawn!(async move { owned.pause_for_restart().await });
            // Observe the real durable commit before releasing the worker.
            timeout(Duration::from_secs(5), async {
                loop {
                    let task = store
                        .current_goal_for_session(&key.durable_id())
                        .await
                        .unwrap()
                        .unwrap();
                    if task.execution_epoch != original.execution_epoch {
                        assert_eq!(task.status, TaskStatus::Paused);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            observation.release.notify_one();
            timeout(Duration::from_secs(5), worker)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            None
        }
        Control::ExternalCancellation => {
            // The transport wrapper is introduced in the later policy slice.
            // Here exercise its committed-fence condition through the existing
            // guarded store and public supervisor drain, without importing it.
            assert_eq!(
                store
                    .pause_session_goal(
                        &original.id,
                        &key.durable_id(),
                        original.execution_epoch,
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
            observation.release.notify_one();
            // A pre-fix worker error is retained for the durable-state assertion
            // below; after repair this must be a clean Fenced result.
            let result = timeout(Duration::from_secs(5), supervisor.drain(&scope))
                .await
                .unwrap();
            if store
                .current_goal_for_session(&key.durable_id())
                .await
                .unwrap()
                .unwrap()
                .status
                == TaskStatus::Paused
            {
                assert_eq!(result.unwrap(), GoalExecutionOutcome::Fenced);
            }
            None
        }
        Control::Budget => {
            let consumed = if window == Window::Verifier { 24 } else { 12 };
            assert_eq!(
                store
                    .update_session_goal_limits(
                        &original.id,
                        &key.durable_id(),
                        original.execution_epoch,
                        Some(consumed),
                        None,
                    )
                    .await
                    .unwrap(),
                GoalTransitionResult::Applied
            );
            observation.release.notify_one();
            assert_eq!(
                timeout(Duration::from_secs(5), supervisor.drain(&scope))
                    .await
                    .unwrap()
                    .unwrap(),
                GoalExecutionOutcome::Fenced
            );
            None
        }
        Control::StillRunning | Control::StillRunningBudget => {
            observation.release.notify_one();
            assert!(
                timeout(Duration::from_secs(5), supervisor.drain(&scope))
                    .await
                    .unwrap()
                    .is_err()
            );
            None
        }
    };
    let task = store
        .current_goal_for_session(&key.durable_id())
        .await
        .unwrap()
        .unwrap();
    let goal = store.get_goal_task(&task.id).await.unwrap().unwrap();
    assert_eq!(task.id, original.id);
    let uncommitted = matches!(control, Control::StillRunning | Control::StillRunningBudget);
    let failed = uncommitted || matches!(refusal, Refusal::Pending | Refusal::Missing);
    let expected = if control == Control::Cancel {
        TaskStatus::Cancelled
    } else if failed {
        TaskStatus::Failed
    } else {
        TaskStatus::Paused
    };
    let events = observation.events.lock().unwrap().clone();
    if control == Control::Budget {
        assert_eq!(observation.budget_fences.load(Ordering::SeqCst), 1);
        assert_eq!(events[0], "host_error:Goal budget is exhausted");
    }
    assert_eq!(
        task.status, expected,
        "{window:?}/{control:?}/{refusal:?}: {events:?}; late={late:?}"
    );
    let operations = if window == Window::Verifier { 2 } else { 1 };
    assert_eq!(observation.gates.load(Ordering::SeqCst), 1);
    assert_eq!(observation.parents.load(Ordering::SeqCst), 1);
    assert_eq!(observation.verifiers.load(Ordering::SeqCst), operations - 1);
    assert_eq!(observation.admissions.load(Ordering::SeqCst), operations);
    assert_eq!(observation.settlements.load(Ordering::SeqCst), operations);
    assert_eq!(
        tracker.get_usage_totals_for_task(&task.id).unwrap().0,
        (operations * 12) as u64
    );
    assert_eq!(
        tracker
            .get_summary_for_task(&task.id)
            .unwrap()
            .request_count,
        operations
    );
    assert!(goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none());
    assert_eq!(
        goal.accounting_state,
        match refusal {
            Refusal::Pending => GoalAccountingState::OutcomeUnknown,
            Refusal::Missing => GoalAccountingState::Missing,
            _ => GoalAccountingState::Complete,
        }
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.starts_with("host_error:"))
            .count(),
        1
    );
    if !failed {
        assert_eq!(
            events.len(),
            1,
            "no duplicate core error or failure notice: {events:?}"
        );
        assert_eq!(task.execution_epoch, original.execution_epoch + 1);
        assert_eq!(
            store
                .terminal_reason_for_session_goal(&task.id, &key.durable_id())
                .await
                .unwrap(),
            None
        );
        if refusal == Refusal::Plain {
            assert_eq!(events, vec!["host_error:fixture untyped error after pause"]);
            let Some(GoalResponse::Paused(paused)) = &late else {
                panic!("the settled ordinary error must preserve the explicit pause response");
            };
            assert_eq!(paused.task_id, original.id);
            assert_eq!(paused.execution_epoch, original.execution_epoch + 1);
            assert_eq!(paused.accounting_state, GoalAccountingState::Complete);
            assert_eq!(paused.pause_reason, Some(GoalPauseReason::OperatorPaused));
        }
        if expected == TaskStatus::Paused {
            assert_eq!(
                goal.pause_reason,
                Some(match control {
                    Control::Restart => GoalPauseReason::DaemonRestart,
                    Control::Budget => GoalPauseReason::BudgetExhausted,
                    _ => GoalPauseReason::OperatorPaused,
                })
            );
            if matches!(control, Control::Pause | Control::Resume) {
                assert!(matches!(late, Some(GoalResponse::Paused(_))));
            }
        }
    } else {
        let reason = store
            .terminal_reason_for_session_goal(&task.id, &key.durable_id())
            .await
            .unwrap();
        assert_eq!(
            reason.as_deref(),
            Some(if uncommitted {
                if window == Window::Parent {
                    "parent_operation_failed"
                } else {
                    "verifier_operation_failed"
                }
            } else if refusal == Refusal::Pending {
                "accounting_outcome_unknown"
            } else {
                assert_eq!(refusal, Refusal::Missing);
                "accounting_missing_or_invalid"
            })
        );
        assert_eq!(
            events.iter().filter(|e| e.starts_with("notice:")).count(),
            1
        );
        if uncommitted {
            assert_eq!(events.len(), 2);
            assert_eq!(
                events[1],
                format!(
                    "notice:{:?}",
                    GoalExecutionNotice::Failed(if window == Window::Parent {
                        GoalFailureReason::AgentOperationFailed
                    } else {
                        GoalFailureReason::VerifierOperationFailed
                    })
                )
            );
        } else {
            assert_eq!(events.len(), 3);
            assert_eq!(
                events,
                vec![
                    "host_error:Goal task is not running".to_owned(),
                    format!("core_error:{}", accounting_failure_chain(refusal)),
                    format!(
                        "notice:{:?}",
                        GoalExecutionNotice::Failed(if refusal == Refusal::Pending {
                            GoalFailureReason::OperationOutcomeUnknown
                        } else {
                            GoalFailureReason::AccountingIncomplete
                        })
                    ),
                ]
            );
        }
    }
    let mut probes = 0;
    let mut terminal_diagnostics = 0;
    loop {
        match logs.try_recv() {
            Ok(event) if event["attributes"]["task_id"] == task.id => {
                if event["attributes"]["fixture_probe"] == true {
                    probes += 1;
                }
                if event["attributes"]["controller_action"] == "terminal_failure" {
                    terminal_diagnostics += 1;
                }
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(error) => panic!("diagnostic capture lost evidence: {error}"),
        }
    }
    assert_eq!(probes, 1);
    if !failed {
        assert_eq!(terminal_diagnostics, 0);
    }
    if control == Control::Resume {
        let command_config = live.handle().snapshot();
        let mut resumed = supervisor
            .submit(
                &GoalHostSettings::from_config(&command_config, 42, "fixture-boot").unwrap(),
                ingress,
                driver,
                GoalCommand::Resume { response: None },
            )
            .await
            .unwrap();
        assert!(matches!(resumed.response(), GoalResponse::Resumed(_)));
        let running = store
            .current_goal_for_session(&key.durable_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(running.id, original.id);
        assert_eq!(running.execution_epoch, task.execution_epoch + 1);
        let resumed_scope =
            GoalExecutionScope::new(&running.id, key.durable_id(), running.execution_epoch)
                .unwrap();
        supervisor
            .launch_after_response(&settings_source, &mut resumed)
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(5), supervisor.drain(&resumed_scope))
                .await
                .unwrap()
                .unwrap(),
            GoalExecutionOutcome::Completed
        );
        assert_eq!(
            store
                .current_goal_for_session(&key.durable_id())
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Completed
        );
        assert_eq!(
            tracker.get_usage_totals_for_task(&original.id).unwrap().0,
            36
        );
        assert_eq!(observation.parents.load(Ordering::SeqCst), 2);
        assert_eq!(observation.verifiers.load(Ordering::SeqCst), 1);
        assert_eq!(observation.admissions.load(Ordering::SeqCst), 3);
        assert_eq!(observation.settlements.load(Ordering::SeqCst), 3);
        assert_eq!(observation.gates.load(Ordering::SeqCst), 1);
    }
}

macro_rules! case {
    ($matrix:ident, $zerocode:ident, $window:ident, $control:ident, $refusal:ident) => {
        #[tokio::test]
        async fn $matrix() {
            run_case(
                GoalSessionKey::matrix("matrix_fixture_refusal").unwrap(),
                Window::$window,
                Control::$control,
                Refusal::$refusal,
            )
            .await;
        }
        #[tokio::test]
        async fn $zerocode() {
            run_case(
                GoalSessionKey::zero_code("fixture-refusal").unwrap(),
                Window::$window,
                Control::$control,
                Refusal::$refusal,
            )
            .await;
        }
    };
}

case!(
    clean_parent_pause,
    zerocode_clean_parent_pause,
    Parent,
    Pause,
    Fenced
);
case!(
    clean_parent_resume,
    zerocode_clean_parent_resume,
    Parent,
    Resume,
    Fenced
);
case!(
    clean_parent_restart,
    zerocode_clean_parent_restart,
    Parent,
    Restart,
    Fenced
);
case!(
    clean_parent_external_cancellation,
    zerocode_clean_parent_external_cancellation,
    Parent,
    ExternalCancellation,
    Fenced
);
case!(
    clean_verifier_pause,
    zerocode_clean_verifier_pause,
    Verifier,
    Pause,
    Fenced
);
case!(clean_cancel, zerocode_clean_cancel, Parent, Cancel, Fenced);
case!(
    settled_untyped_parent_error_preserves_operator_pause,
    zerocode_settled_untyped_parent_error_preserves_operator_pause,
    Parent,
    Pause,
    Plain
);
case!(
    pending_usage_remains_terminal,
    zerocode_pending_usage_remains_terminal,
    Parent,
    Pause,
    Pending
);
case!(
    missing_usage_remains_terminal,
    zerocode_missing_usage_remains_terminal,
    Parent,
    Pause,
    Missing
);
case!(
    marked_parent_error_while_running_remains_terminal,
    zerocode_marked_parent_error_while_running_remains_terminal,
    Parent,
    StillRunning,
    Fenced
);
case!(
    marked_verifier_error_while_running_remains_terminal,
    zerocode_marked_verifier_error_while_running_remains_terminal,
    Verifier,
    StillRunning,
    Fenced
);
case!(
    committed_parent_budget_pause_survives_drain,
    zerocode_committed_parent_budget_pause_survives_drain,
    Parent,
    Budget,
    Fenced
);
case!(
    committed_verifier_budget_pause_survives_drain,
    zerocode_committed_verifier_budget_pause_survives_drain,
    Verifier,
    Budget,
    Fenced
);
case!(
    forged_parent_budget_marker_remains_terminal,
    zerocode_forged_parent_budget_marker_remains_terminal,
    Parent,
    StillRunningBudget,
    Fenced
);
case!(
    forged_verifier_budget_marker_remains_terminal,
    zerocode_forged_verifier_budget_marker_remains_terminal,
    Verifier,
    StillRunningBudget,
    Fenced
);
