//! Real-store tests of retained and abandoned cutovers, without a global plane.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use tokio::{
    sync::Notify,
    time::{Duration, timeout},
};
use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
use zeroclaw_config::{cost::CostTracker, schema::Config};
use zeroclaw_goal::{
    GoalExecutionNotice, GoalExecutionRestartCoordinator, GoalExecutionScope,
    GoalHostSettingsSource, GoalIngressContext, GoalIngressPrincipal, GoalOperationScope,
    GoalParentTurn, GoalParentTurnCheckpoint, GoalParentTurnResult, GoalResponse, GoalRuntime,
    GoalSessionBinding, GoalSessionDriver, GoalSessionExecutionLease, GoalSessionKey,
    GoalSessionLease, GoalVerifierResult, GoalVerifierTurn,
};

use crate::{
    control_plane::{GoalTaskRegistry, SqliteTaskStore, TaskStatus},
    goal_accounting::{ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory},
};

struct CutoverDriver {
    binding: GoalSessionBinding,
    started: Arc<Notify>,
    release: Arc<Notify>,
    verifier_calls: Arc<AtomicUsize>,
    notices: Arc<AtomicUsize>,
    checkpoint: Arc<std::sync::Mutex<Option<GoalParentTurnCheckpoint>>>,
}

struct CutoverLease {
    session_key: GoalSessionKey,
    started: Arc<Notify>,
    release: Arc<Notify>,
    verifier_calls: Arc<AtomicUsize>,
    notices: Arc<AtomicUsize>,
}

fn fixture_config(enabled: bool) -> Config {
    let mut config = Config::default();
    config.goal.enabled = enabled;
    config.goal.default_token_limit = Some(100);
    config.goal.default_cost_limit_usd = Some(1.0);
    config.goal.verifier.model_provider =
        zeroclaw_config::providers::ModelProviderRef::new("openai.fixture");
    config.providers.models.openai.insert(
        "fixture".into(),
        zeroclaw_config::schema::OpenAIModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                model: Some("fixture-model".into()),
                ..Default::default()
            },
        },
    );
    config.agents.insert("main".into(), Default::default());
    config
}

#[async_trait]
impl GoalSessionDriver for CutoverDriver {
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
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> anyhow::Result<Box<dyn GoalSessionExecutionLease>> {
        *self.checkpoint.lock().unwrap() = Some(checkpoint.clone());
        Ok(Box::new(CutoverLease {
            session_key: self.binding.session_key().clone(),
            started: self.started.clone(),
            release: self.release.clone(),
            verifier_calls: self.verifier_calls.clone(),
            notices: self.notices.clone(),
        }))
    }
}

#[async_trait]
impl GoalSessionExecutionLease for CutoverLease {
    fn session_key(&self) -> &GoalSessionKey {
        &self.session_key
    }

    async fn canonical_history(
        &mut self,
        _input: &zeroclaw_goal::control::GoalParentTurnInput,
    ) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ChatMessage>> {
        Ok(Vec::new())
    }

    async fn run_parent_turn(
        &mut self,
        _operation: &GoalOperationScope,
        turn: GoalParentTurn,
    ) -> anyhow::Result<GoalParentTurnResult> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(GoalParentTurnResult::from_continuation_prefix(
            "settled candidate".into(),
            turn.working_history,
        ))
    }

    async fn run_verifier(
        &mut self,
        _operation: &GoalOperationScope,
        _turn: GoalVerifierTurn,
    ) -> anyhow::Result<GoalVerifierResult> {
        self.verifier_calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("a revoked worker must not admit a verifier")
    }

    async fn append_parent_candidate(&mut self, _candidate: String) -> anyhow::Result<()> {
        Ok(())
    }

    async fn publish_goal_notice(&mut self, _notice: GoalExecutionNotice) -> anyhow::Result<()> {
        self.notices.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug)]
struct OriginalCutoverSaveFailure;

impl std::fmt::Display for OriginalCutoverSaveFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("distinctive post-cancellation config save failure")
    }
}

impl std::error::Error for OriginalCutoverSaveFailure {}

#[derive(Clone, Copy)]
enum WorkerDisposition {
    FinishSuccess,
    SaveFailure,
    Abandoned,
    Shutdown,
}

#[tokio::test]
async fn retained_live_policy_cutover_drains_before_releasing_generation_write() {
    assert_worker_disposition(WorkerDisposition::FinishSuccess).await;
}

#[tokio::test]
async fn retained_live_policy_cutover_drains_before_reopening_after_save_failure() {
    assert_worker_disposition(WorkerDisposition::SaveFailure).await;
}

#[tokio::test]
async fn abandoned_live_policy_cutover_stays_closed_until_explicit_lifecycle_recovery() {
    assert_worker_disposition(WorkerDisposition::Abandoned).await;
}

#[tokio::test]
async fn graceful_shutdown_pauses_before_waiting_and_observes_admitted_parent_settlement() {
    assert_worker_disposition(WorkerDisposition::Shutdown).await;
}

async fn assert_worker_disposition(disposition: WorkerDisposition) {
    let abandoned = matches!(disposition, WorkerDisposition::Abandoned);
    let shutdown = matches!(disposition, WorkerDisposition::Shutdown);
    let save_failure = matches!(disposition, WorkerDisposition::SaveFailure);
    timeout(Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
        let registry = store.clone() as Arc<dyn GoalTaskRegistry>;
        let runtime = GoalRuntime::new(registry.clone());
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
                registry.clone(),
                tracker,
                "main",
                Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
            )
            .unwrap(),
        );
        let coordinator = Arc::new(GoalExecutionRestartCoordinator::new());
        let config = fixture_config(true);
        let authority =
            crate::live_config_authority::LiveConfigAuthority::for_tests(config.clone());
        let live = authority.live_handle();
        let source = GoalHostSettingsSource::new(live.clone(), 42, "test-boot").unwrap();
        let engine = Arc::new(runtime.execution_engine(factory));
        let supervisor = coordinator
            .new_supervisor(engine.clone(), source.clone())
            .await;
        let ingress = GoalIngressContext::trusted(
            GoalSessionKey::zero_code("abandoned-cutover").unwrap(),
            "main",
            "zerocode:abandoned-cutover",
            GoalIngressPrincipal::ZeroCode {
                tui_id: "test-owner".into(),
            },
        )
        .unwrap();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let verifier_calls = Arc::new(AtomicUsize::new(0));
        let notices = Arc::new(AtomicUsize::new(0));
        let checkpoint = Arc::new(std::sync::Mutex::new(None));
        let driver = Arc::new(CutoverDriver {
            binding: GoalSessionBinding::new(ingress.session_key().clone()),
            started: started.clone(),
            release: release.clone(),
            verifier_calls: verifier_calls.clone(),
            notices: notices.clone(),
            checkpoint: checkpoint.clone(),
        });
        let mut execution = supervisor
            .submit(
                &source.snapshot().settings(),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "finish the bounded repair".into(),
                },
            )
            .await
            .unwrap();
        supervisor
            .launch_after_response(&source, &mut execution)
            .await
            .unwrap();
        started.notified().await;
        let before = registry
            .current_goal_for_session(&ingress.session_key().durable_id())
            .await
            .unwrap()
            .unwrap();
        let scope = GoalExecutionScope::new(
            before.id.clone(),
            ingress.session_key().durable_id(),
            before.execution_epoch,
        )
        .unwrap();
        let mut prospective = config.clone();
        prospective.goal.enabled = false;
        let mut cutover = if shutdown {
            None
        } else {
            Some(
                super::begin_live_goal_policy_cutover_with(
                    coordinator.clone(),
                    registry.clone(),
                    &live,
                    &prospective,
                )
                .await
                .unwrap(),
            )
        };
        assert!(
            live.snapshot().goal.enabled,
            "the abandoned request has not swapped config"
        );
        assert!(
            supervisor.owns_scope(&scope).await,
            "an admitted operation must not be interrupted"
        );

        let checkpoint = checkpoint.lock().unwrap().clone().unwrap();
        if let Some(cutover) = cutover.as_mut() {
            let commit = authority.begin_config_commit().await.unwrap();
            cutover.release_config_commit(commit);
            assert!(!authority.config_write_lock_is_held());
            assert_eq!(authority.agent_lifecycle().active_config_write_count(), 1);
        }
        if abandoned {
            drop(cutover.take());
            assert_eq!(authority.agent_lifecycle().active_config_write_count(), 0);
            assert!(coordinator.reopen_for_generation_checked().await.is_err());
        }
        let drain = async {
            if let Some(cutover) = cutover {
                let primary = save_failure.then(|| OriginalCutoverSaveFailure.into());
                let result = cutover.finish(false, primary).await;
                if save_failure {
                    let error = result.unwrap_err();
                    assert!(
                        error
                            .chain()
                            .any(|cause| cause.is::<OriginalCutoverSaveFailure>())
                    );
                    assert!(format!("{error:#}").contains("cancelled 1 goal(s)"));
                } else {
                    result.unwrap();
                }
            } else {
                let exit = super::settle_prepared_goal_exit(
                    Ok(super::PreparedDaemonExit::shutdown()),
                    Some(coordinator.clone()),
                )
                .await
                .unwrap();
                assert!(matches!(exit, super::DaemonExit::Shutdown));
            }
        };
        tokio::pin!(drain);
        tokio::select! {
            biased;
            () = &mut drain => panic!("cutover completed before the admitted parent settled"),
            () = checkpoint.wait_closed() => {}
        }
        // Both production paths fence durably before the admitted parent may
        // settle. A cancellation transaction wins over ordinary restart pause.
        let fenced = registry
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fenced.status,
            if shutdown {
                TaskStatus::Paused
            } else {
                TaskStatus::Cancelled
            }
        );
        assert_eq!(fenced.execution_epoch, before.execution_epoch + 1);
        if shutdown {
            assert_eq!(
                registry
                    .get_goal_task(scope.task_id())
                    .await
                    .unwrap()
                    .unwrap()
                    .pause_reason,
                Some(zeroclaw_goal::GoalPauseReason::DaemonRestart)
            );
        } else {
            assert_eq!(
                registry
                    .terminal_reason_for_session_goal(scope.task_id(), scope.session_id())
                    .await
                    .unwrap()
                    .as_deref(),
                Some("policy_revoked")
            );
        }
        assert!(supervisor.owns_scope(&scope).await);
        assert!(coordinator.reopen_for_generation_checked().await.is_err());
        assert_eq!(
            authority.agent_lifecycle().active_config_write_count(),
            usize::from(!abandoned && !shutdown)
        );
        assert!(!authority.config_write_lock_is_held());
        release.notify_one();
        drain.await;
        assert_eq!(authority.agent_lifecycle().active_config_write_count(), 0);
        assert_eq!(verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(notices.load(Ordering::SeqCst), 0);
        let settled = registry
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settled.status, fenced.status);
        assert_eq!(settled.execution_epoch, fenced.execution_epoch);

        if abandoned || shutdown {
            // Only actual quiescence permits a fresh reconciled generation to
            // clear the abandoned closure; dropping the request never does.
            coordinator.reopen_for_generation_checked().await.unwrap();
        }

        // Quiescence may have atomically retired the old host. A fresh host
        // receives the same canonical store; reusing a retired Arc is invalid.
        let supervisor = coordinator.new_supervisor(engine, source.clone()).await;

        let resume = supervisor
            .submit(
                &source.snapshot().settings(),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Resume { response: None },
            )
            .await
            .unwrap();
        if shutdown {
            assert!(matches!(resume.response(), GoalResponse::Resumed(_)));
        } else {
            assert!(matches!(
                resume.response(),
                GoalResponse::Terminal(_) | GoalResponse::AlreadyCancelled(_)
            ));
        }
        drop(resume);
        if shutdown {
            let cancelled = supervisor
                .submit(
                    &source.snapshot().settings(),
                    ingress.clone(),
                    driver.clone(),
                    GoalCommand::Cancel,
                )
                .await
                .unwrap();
            assert!(matches!(cancelled.response(), GoalResponse::Cancelled(_)));
            drop(cancelled);
        }

        // Successful admission, not just a counter increment, proves the gate reopened.
        let successor = supervisor
            .submit(
                &source.snapshot().settings(),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "start a new bounded repair".into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(successor.response(), GoalResponse::Started(_)));
        drop(successor);
        let cleanup = supervisor
            .submit(
                &source.snapshot().settings(),
                ingress,
                driver,
                GoalCommand::Cancel,
            )
            .await
            .unwrap();
        assert!(matches!(cleanup.response(), GoalResponse::Cancelled(_)));
    })
    .await
    .expect("the exact cutover duty must remain retained until explicit drain");
}

#[tokio::test]
async fn published_policy_expansion_blocks_new_goal_until_reconciled_replacement() {
    timeout(Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
        let registry = store.clone() as Arc<dyn GoalTaskRegistry>;
        let runtime = GoalRuntime::new(registry.clone());
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
                registry.clone(),
                tracker,
                "main",
                Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
            )
            .unwrap(),
        );
        let engine = Arc::new(runtime.execution_engine(factory));
        let coordinator = Arc::new(GoalExecutionRestartCoordinator::new());
        let old_config = fixture_config(false);
        let prospective = fixture_config(true);
        let live = zeroclaw_config::live::LiveConfig::new(old_config);
        let source = GoalHostSettingsSource::new(live.handle(), 42, "fixture-boot").unwrap();
        let supervisor = coordinator
            .new_supervisor(engine.clone(), source.clone())
            .await;
        let ingress = GoalIngressContext::trusted(
            GoalSessionKey::zero_code("expanded-scope").unwrap(),
            "main",
            "zerocode:expanded-scope",
            GoalIngressPrincipal::ZeroCode {
                tui_id: "fixture-controller".into(),
            },
        )
        .unwrap();
        let driver = Arc::new(CutoverDriver {
            binding: GoalSessionBinding::new(ingress.session_key().clone()),
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            verifier_calls: Arc::new(AtomicUsize::new(0)),
            notices: Arc::new(AtomicUsize::new(0)),
            checkpoint: Arc::new(std::sync::Mutex::new(None)),
        });
        let cutover = super::begin_live_goal_policy_cutover_with(
            coordinator.clone(),
            registry.clone(),
            &live.handle(),
            &prospective,
        )
        .await
        .unwrap();
        assert_eq!(cutover.cancelled_goal_count(), Some(0));
        live.publish(live.next_revision().unwrap(), prospective.clone())
            .unwrap();
        cutover.finish(true, None).await.unwrap();
        let start = GoalCommand::Start {
            budget: GoalBudgetSelection::Defaults,
            objective: "new explicitly authorized Goal".into(),
        };
        assert!(
            supervisor
                .submit(
                    &source.snapshot().settings(),
                    ingress.clone(),
                    driver.clone(),
                    start.clone(),
                )
                .await
                .is_err()
        );
        assert!(
            registry
                .current_goal_for_session(&ingress.session_key().durable_id())
                .await
                .unwrap()
                .is_none()
        );
        coordinator.quiesce_for_restart().await.unwrap();
        let next_live = zeroclaw_config::live::LiveConfig::new(prospective);
        let next_source =
            GoalHostSettingsSource::new(next_live.handle(), 42, "fixture-boot").unwrap();
        // Explicit generation reconciliation uses the same real classifier and
        // registry before the checked replacement reopening.
        zeroclaw_goal::control::revoke_goals_under_policy(
            registry.as_ref(),
            &super::goal_policy_snapshot(&next_live.snapshot()),
        )
        .await
        .unwrap();
        coordinator.reopen_for_generation_checked().await.unwrap();
        let next_host = coordinator
            .new_supervisor(engine, next_source.clone())
            .await;
        let started = next_host
            .submit(
                &next_source.snapshot().settings(),
                ingress.clone(),
                driver.clone(),
                start,
            )
            .await
            .unwrap();
        assert!(matches!(started.response(), GoalResponse::Started(_)));
        drop(started);
        coordinator.quiesce_for_restart().await.unwrap();
        assert_eq!(driver.verifier_calls.load(Ordering::SeqCst), 0);
    })
    .await
    .expect("published expansion must wait for explicit runtime replacement");
}
