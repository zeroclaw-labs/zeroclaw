//! R8 / BR62: ordinary controls are deferred; immediate controls are not.
//! These component tests do not substitute for the real surface-driver gates.

use std::{sync::Arc, time::Duration};

use anyhow::{Result, bail};
use async_trait::async_trait;
use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
use zeroclaw_config::{
    goal::{GoalConfig, GoalVerifierConfig},
    live::LiveConfig,
    providers::ModelProviderRef,
    schema::{Config, ModelProviderConfig, OpenAIModelProviderConfig},
};
use zeroclaw_goal::{
    GoalControlAdmission, GoalControlClass, GoalDeferredControl, GoalExecutionScope,
    GoalExecutionSupervisor, GoalHostSettings, GoalHostSettingsSource, GoalIngressContext,
    GoalIngressPrincipal, GoalParentTurnCheckpoint, GoalQueuedControl, GoalResponse, GoalRuntime,
    GoalSessionBinding, GoalSessionDriver, GoalSessionExecutionLease, GoalSessionKey,
    GoalSessionLease, GoalTurnBoundary, classify_goal_control,
};

mod support;
#[path = "support/worker.rs"]
mod worker;

use support::{IntentState, NoModelAccounting, Registry};
use zeroclaw_api::task::TaskStatus;
use zeroclaw_goal::task::GoalControlIntentSettlement;

fn goal_config_with(
    default_tokens: Option<u64>,
    default_cost: Option<f64>,
    verifier: &str,
    enabled: bool,
) -> Config {
    let mut config = Config {
        goal: GoalConfig {
            enabled,
            default_token_limit: default_tokens,
            default_cost_limit_usd: default_cost,
            include_channel_turn_context: true,
            verifier: GoalVerifierConfig {
                model_provider: ModelProviderRef::new(verifier),
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
    config
}

fn goal_config() -> Config {
    goal_config_with(Some(1000), Some(0.0), "openai.default", true)
}

fn managed_config() -> Config {
    let mut config = goal_config();
    config
        .agents
        .insert("fixture-agent".into(), Default::default());
    config
}

fn settings(config: &Config) -> GoalHostSettings<'_> {
    GoalHostSettings::from_config(config, 1, "fixture-boot").unwrap()
}

fn source(live: &LiveConfig) -> GoalHostSettingsSource {
    GoalHostSettingsSource::new(live.handle(), 1, "fixture-boot").unwrap()
}

fn supervisor(registry: Arc<Registry>) -> Arc<GoalExecutionSupervisor> {
    let runtime = GoalRuntime::new(registry);
    Arc::new(GoalExecutionSupervisor::new(Arc::new(
        runtime.execution_engine(Arc::new(NoModelAccounting)),
    )))
}

fn queued(admission: GoalControlAdmission, expected: GoalQueuedControl) -> GoalDeferredControl {
    match admission {
        GoalControlAdmission::Queued { response, deferred } => {
            assert_eq!(response, GoalResponse::Queued(expected));
            deferred
        }
        GoalControlAdmission::Direct(_) => panic!("held ordinary turn must defer this control"),
    }
}

async fn retire_without_waiting(supervisor: &GoalExecutionSupervisor) -> bool {
    tokio::time::timeout(Duration::from_secs(1), supervisor.try_retire())
        .await
        .expect("retirement must not wait for command or boundary ownership")
}

#[tokio::test]
async fn deferred_start_rechecks_commit_config_and_persists_fresh_defaults() {
    use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};

    for verifier in ["openai.missing", "openai.default"] {
        let live = LiveConfig::new(goal_config());
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        let supervisor = supervisor(registry.clone());
        let ordinary = driver.boundary.clone().lock_owned().await;
        let snapshot = live.handle().snapshot();
        assert_eq!(snapshot.goal.default_token_limit, Some(1000));
        let deferred = queued(
            supervisor
                .admit_control(
                    &settings(&snapshot),
                    ingress,
                    driver.clone(),
                    launch_command(false),
                )
                .await
                .unwrap(),
            GoalQueuedControl::Start,
        );
        {
            let state = registry.0.lock().unwrap();
            assert!(state.task.is_none());
            assert!(state.transitions.is_empty());
            assert_eq!(state.intents.len(), 1);
            assert_eq!(state.intents[0].1, IntentState::Pending);
        }
        drop(ordinary);
        let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
            .await
            .unwrap()
            .unwrap();
        live.publish(
            live.next_revision().unwrap(),
            goal_config_with(Some(2000), Some(0.0), verifier, true),
        )
        .unwrap();
        let snapshot = live.handle().snapshot();
        let result = ready.commit(&settings(&snapshot)).await;
        if verifier == "openai.missing" {
            let error = match result {
                Ok(_) => panic!("dangling commit config must reject the deferred Start"),
                Err(error) => error,
            };
            let structured = ConfigApiError::from_validation(error);
            assert_eq!(structured.code, ConfigApiCode::DanglingReference);
            assert_eq!(
                structured.path.as_deref(),
                Some("goal.verifier.model_provider")
            );
            assert!(driver.command.try_lock().is_ok());
            assert!(driver.boundary.try_lock().is_ok());
            assert!(matches!(
                wait_for_settlement(&registry).await,
                IntentState::Settled(GoalControlIntentSettlement::Abandoned { .. })
            ));
            let state = registry.0.lock().unwrap();
            assert!(state.task.is_none());
            assert!(state.goal.is_none());
            assert!(state.transitions.is_empty());
            assert_eq!(state.intents.len(), 1);
        } else {
            let committed = result.unwrap();
            assert!(matches!(committed.response(), GoalResponse::Started(_)));
            {
                let state = registry.0.lock().unwrap();
                assert_eq!(state.transitions, ["start"]);
                assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
                assert_eq!(
                    state.goal.as_ref().unwrap().effective_token_limit,
                    Some(2000)
                );
                assert_ne!(
                    state.goal.as_ref().unwrap().effective_token_limit,
                    Some(1000)
                );
            }
            assert_eq!(
                wait_for_settlement(&registry).await,
                IntentState::Settled(GoalControlIntentSettlement::Applied)
            );
            let mut submission = committed.release();
            supervisor
                .pause_before_launch(
                    &mut submission,
                    zeroclaw_goal::GoalPauseReason::OperatorPaused,
                )
                .await
                .unwrap();
            drop(submission);
            assert!(driver.command.try_lock().is_ok());
            assert!(driver.boundary.try_lock().is_ok());
        }
    }
}

#[tokio::test]
async fn start_without_budget_flags_or_defaults_returns_budget_required() {
    let config = goal_config_with(None, None, "openai.default", true);
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());

    let GoalControlAdmission::Direct(submission) = supervisor
        .admit_control(
            &settings(&config),
            ingress,
            driver.clone(),
            launch_command(false),
        )
        .await
        .unwrap()
    else {
        panic!("a Start without defaults must not be queued")
    };
    assert_eq!(submission.into_response(), GoalResponse::BudgetRequired);

    let state = registry.0.lock().unwrap();
    assert!(state.task.is_none());
    assert!(state.goal.is_none());
    assert!(state.transitions.is_empty());
    assert!(state.intents.is_empty());
    assert_eq!(state.session_lookups, 0);
    assert_eq!(
        driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn explicit_command_budgets_start_without_configured_defaults() {
    use zeroclaw_commands::goal::GoalBudgetLimits;

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
        (GoalBudgetSelection::Unlimited, None, None),
    ] {
        let config = goal_config_with(None, None, "openai.default", true);
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        let supervisor = supervisor(registry.clone());
        let GoalControlAdmission::Direct(mut submission) = supervisor
            .admit_control(
                &settings(&config),
                ingress,
                driver,
                GoalCommand::Start {
                    budget,
                    objective: "Start with an explicit budget".into(),
                },
            )
            .await
            .unwrap()
        else {
            panic!("an immediately available Start must be direct")
        };
        let GoalResponse::Started(started) = submission.response() else {
            panic!("explicit budget selection must start")
        };
        assert_eq!(started.token_limit, expected_tokens);
        assert_eq!(started.cost_limit_usd, expected_cost);
        {
            let state = registry.0.lock().unwrap();
            let goal = state.goal.as_ref().expect("Start persists the Goal");
            assert_eq!(goal.effective_token_limit, expected_tokens);
            assert_eq!(goal.effective_cost_limit_usd, expected_cost);
        }
        supervisor
            .pause_before_launch(
                &mut submission,
                zeroclaw_goal::GoalPauseReason::OperatorPaused,
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn single_positive_default_starts_with_that_limit() {
    let config = goal_config_with(Some(500), None, "openai.default", true);
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    let GoalControlAdmission::Direct(mut submission) = supervisor
        .admit_control(&settings(&config), ingress, driver, launch_command(false))
        .await
        .unwrap()
    else {
        panic!("an immediately available Start must be direct")
    };

    let GoalResponse::Started(started) = submission.response() else {
        panic!("the positive configured default must start")
    };
    assert_eq!(started.token_limit, Some(500));
    assert_eq!(started.cost_limit_usd, None);
    {
        let state = registry.0.lock().unwrap();
        let goal = state.goal.as_ref().expect("Start persists the Goal");
        assert_eq!(goal.effective_token_limit, Some(500));
        assert_eq!(goal.effective_cost_limit_usd, None);
    }

    supervisor
        .pause_before_launch(
            &mut submission,
            zeroclaw_goal::GoalPauseReason::OperatorPaused,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn zero_token_default_without_cost_default_returns_budget_required() {
    let config = goal_config_with(Some(0), None, "openai.default", true);
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    let GoalControlAdmission::Direct(submission) = supervisor
        .admit_control(&settings(&config), ingress, driver, launch_command(false))
        .await
        .unwrap()
    else {
        panic!("an unconfigured default must not be queued")
    };
    assert_eq!(submission.into_response(), GoalResponse::BudgetRequired);

    let state = registry.0.lock().unwrap();
    assert!(state.task.is_none());
    assert!(state.goal.is_none());
    assert!(state.transitions.is_empty());
    assert_eq!(state.session_lookups, 0);
}

#[tokio::test]
async fn invalid_defaults_reject_explicit_budget_start_with_typed_path() {
    use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};

    let config = goal_config_with(Some(500), Some(-1.0), "openai.default", true);
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    let error = supervisor
        .admit_control(
            &settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Limits(zeroclaw_commands::goal::GoalBudgetLimits {
                    token_limit: Some(10),
                    cost_limit_usd: None,
                }),
                objective: "Explicit budgets do not bypass config validation".into(),
            },
        )
        .await
        .unwrap_err();
    let structured = ConfigApiError::from_validation(error);
    assert_eq!(structured.code, ConfigApiCode::InvalidNumericRange);
    assert_eq!(
        structured.path.as_deref(),
        Some("goal.default_cost_limit_usd")
    );
    assert_eq!(
        driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    let state = registry.0.lock().unwrap();
    assert!(state.task.is_none());
    assert!(state.goal.is_none());
    assert!(state.transitions.is_empty());
    assert!(state.intents.is_empty());
    assert_eq!(state.session_lookups, 0);
}

#[tokio::test]
async fn deferred_start_with_defaults_removed_before_commit_returns_budget_required() {
    let live = LiveConfig::new(goal_config());
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    let ordinary = driver.boundary.clone().lock_owned().await;
    let snapshot = live.handle().snapshot();
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&snapshot),
                ingress,
                driver.clone(),
                launch_command(false),
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    {
        let state = registry.0.lock().unwrap();
        assert!(state.task.is_none());
        assert!(state.transitions.is_empty());
        assert_eq!(state.intents.len(), 1);
        assert_eq!(state.intents[0].1, IntentState::Pending);
    }

    drop(ordinary);
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    live.publish(
        live.next_revision().unwrap(),
        goal_config_with(None, None, "openai.default", true),
    )
    .unwrap();
    let snapshot = live.handle().snapshot();
    let committed = ready.commit(&settings(&snapshot)).await.unwrap();
    assert_eq!(committed.response(), &GoalResponse::BudgetRequired);
    drop(committed.release());

    let state = registry.0.lock().unwrap();
    assert!(state.task.is_none());
    assert!(state.goal.is_none());
    assert!(state.transitions.is_empty());
    assert_eq!(state.intents.len(), 1);
    assert!(matches!(state.intents[0].1, IntentState::Settled(_)));
    // Intake and the deferred boundary each revalidate the session binding.
    assert_eq!(
        driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
}

#[tokio::test]
async fn deferred_intake_checks_execution_config_but_preserves_admin_controls() {
    use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};

    let live = LiveConfig::new(goal_config_with(
        Some(1000),
        Some(0.0),
        "openai.missing",
        true,
    ));
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
    let execution_supervisor = supervisor(registry.clone());
    for command in [launch_command(false), launch_command(true)] {
        let snapshot = live.handle().snapshot();
        let error = execution_supervisor
            .admit_control(
                &settings(&snapshot),
                ingress.clone(),
                driver.clone(),
                command,
            )
            .await
            .unwrap_err();
        let structured = ConfigApiError::from_validation(error);
        assert_eq!(structured.code, ConfigApiCode::DanglingReference);
        assert_eq!(
            structured.path.as_deref(),
            Some("goal.verifier.model_provider")
        );
        assert_eq!(
            driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        let state = registry.0.lock().unwrap();
        assert!(state.intents.is_empty());
        assert!(state.transitions.is_empty());
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Paused);
        assert_eq!(state.task.as_ref().unwrap().execution_epoch, 1);
    }
    live.publish(
        live.next_revision().unwrap(),
        goal_config_with(Some(1000), Some(0.0), "openai.missing", false),
    )
    .unwrap();
    let snapshot = live.handle().snapshot();
    let GoalControlAdmission::Direct(disabled) = execution_supervisor
        .admit_control(
            &settings(&snapshot),
            ingress,
            driver.clone(),
            launch_command(false),
        )
        .await
        .unwrap()
    else {
        panic!("disabled Goal Mode must return directly");
    };
    assert_eq!(disabled.response(), &GoalResponse::Disabled);
    drop(disabled);
    assert_eq!(
        driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    {
        let state = registry.0.lock().unwrap();
        assert!(state.intents.is_empty());
        assert!(state.transitions.is_empty());
        assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Paused);
        assert_eq!(state.task.as_ref().unwrap().execution_epoch, 1);
    }

    for (command, kind) in [
        (GoalCommand::Status, 0),
        (GoalCommand::Pause { immediate: false }, 1),
        (GoalCommand::Cancel, 2),
    ] {
        let live = LiveConfig::new(goal_config_with(
            Some(1000),
            Some(0.0),
            "openai.missing",
            true,
        ));
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        registry.seed(&ingress.session_key().durable_id(), TaskStatus::Running);
        let supervisor = supervisor(registry.clone());
        let ordinary = driver.boundary.clone().lock_owned().await;
        let snapshot = live.handle().snapshot();
        let admission = tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.admit_control(&settings(&snapshot), ingress, driver.clone(), command),
        )
        .await
        .expect("admin intake must not wait for the ordinary turn")
        .unwrap();
        match (kind, admission) {
            (0, GoalControlAdmission::Direct(submission)) => {
                assert!(matches!(submission.response(), GoalResponse::Status(_)));
                drop(submission);
                assert!(registry.0.lock().unwrap().intents.is_empty());
            }
            (1, admission) => {
                let deferred = queued(admission, GoalQueuedControl::Pause);
                assert!(registry.0.lock().unwrap().transitions.is_empty());
                drop(deferred);
                assert!(matches!(
                    wait_for_settlement(&registry).await,
                    IntentState::Settled(GoalControlIntentSettlement::Abandoned { .. })
                ));
            }
            (2, GoalControlAdmission::Direct(submission)) => {
                assert!(matches!(
                    submission.response(),
                    GoalResponse::Cancelled(_) | GoalResponse::TransitionPending(_)
                ));
                drop(submission);
                assert!(registry.0.lock().unwrap().intents.is_empty());
            }
            _ => panic!("admin control must retain its ordinary classification"),
        }
        assert!(driver.command.try_lock().is_ok());
        drop(ordinary);
        assert!(driver.boundary.try_lock().is_ok());
    }
}

#[tokio::test]
async fn custody_retired_supervisor_rejects_before_any_admission_effect() {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    assert!(retire_without_waiting(&supervisor).await);
    for command in [GoalCommand::Status, launch_command(false)] {
        let error = supervisor
            .admit_control(&settings(&config), ingress.clone(), driver.clone(), command)
            .await
            .unwrap_err();
        assert!(error.is::<zeroclaw_goal::GoalSupervisorRetired>());
    }
    let error = supervisor
        .submit(
            &settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalSupervisorRetired>());
    let state = registry.0.lock().unwrap();
    assert_eq!(state.session_lookups, 0);
    assert!(state.intents.is_empty());
    assert!(state.transitions.is_empty());
    assert_eq!(
        driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn custody_pending_control_stays_owned_through_ready_commit_and_release() {
    let config = goal_config();
    let (_, _, _, supervisor, ordinary, deferred) = queued_pause_fixture().await;
    assert!(!retire_without_waiting(&supervisor).await);
    drop(ordinary);
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!retire_without_waiting(&supervisor).await);
    let committed = ready.commit(&settings(&config)).await.unwrap();
    assert!(!retire_without_waiting(&supervisor).await);
    drop(committed.release());
    // No asynchronous pending-map cleanup should be needed after the waiter
    // owns both the command lease and the ordinary boundary.
    assert!(retire_without_waiting(&supervisor).await);
}

#[tokio::test]
async fn custody_launch_pending_prevents_retirement_until_pause_settles() {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry);
    let GoalControlAdmission::Direct(mut submission) = supervisor
        .admit_control(&settings(&config), ingress, driver, launch_command(false))
        .await
        .unwrap()
    else {
        panic!("idle start must apply directly");
    };
    assert!(!retire_without_waiting(&supervisor).await);
    supervisor
        .pause_before_launch(
            &mut submission,
            zeroclaw_goal::GoalPauseReason::OperatorPaused,
        )
        .await
        .unwrap();
    assert!(retire_without_waiting(&supervisor).await);
}

#[tokio::test]
async fn custody_admission_waiting_for_bind_prevents_retirement() {
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry);
    let command_lease = driver.command.clone().lock_owned().await;
    let admission = {
        let supervisor = supervisor.clone();
        let driver = driver.clone();
        zeroclaw_spawn::spawn!(async move {
            let config = goal_config();
            supervisor
                .admit_control(&settings(&config), ingress, driver, GoalCommand::Status)
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!retire_without_waiting(&supervisor).await);
    drop(command_lease);
    drop(
        tokio::time::timeout(Duration::from_secs(1), admission)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    assert!(retire_without_waiting(&supervisor).await);
}

#[tokio::test]
async fn custody_disposal_closes_a_pending_control_without_waiting_for_its_boundary() {
    let (_, _, registry, supervisor, ordinary, deferred) = queued_pause_fixture().await;
    let session = registry
        .0
        .lock()
        .unwrap()
        .task
        .as_ref()
        .unwrap()
        .session_id
        .clone()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), supervisor.dispose_session(&session))
        .await
        .unwrap()
        .unwrap();
    assert!(!retire_without_waiting(&supervisor).await);
    let closed = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(closed.response(), &GoalResponse::NoCurrentGoal);
    assert!(retire_without_waiting(&supervisor).await);
    drop(ordinary);
}

#[tokio::test]
async fn ready_abandon_reason_is_preserved_after_boundary_acquisition() {
    let (_, _, registry, _, ordinary, deferred) = queued_pause_fixture().await;
    let deferred =
        deferred.with_abandon_reason(zeroclaw_goal::GoalControlAbandonReason::ConnectionClosed);
    drop(ordinary);
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    drop(ready.with_abandon_reason(zeroclaw_goal::GoalControlAbandonReason::ControlDropped));
    assert_eq!(
        wait_for_settlement(&registry).await,
        IntentState::Settled(GoalControlIntentSettlement::Abandoned {
            reason: "control_dropped".into(),
        })
    );
}

#[tokio::test]
async fn idle_resume_pause_and_budget_apply_directly_under_the_boundary() {
    let config = goal_config();
    for (status, command, transition) in [
        (
            TaskStatus::Paused,
            GoalCommand::Resume { response: None },
            "resume",
        ),
        (
            TaskStatus::Running,
            GoalCommand::Pause { immediate: false },
            "pause",
        ),
        (
            TaskStatus::Running,
            GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
            "budget",
        ),
    ] {
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        registry.seed(&ingress.session_key().durable_id(), status);
        let supervisor = supervisor(registry.clone());
        let GoalControlAdmission::Direct(mut submission) = supervisor
            .admit_control(&settings(&config), ingress, driver.clone(), command)
            .await
            .unwrap()
        else {
            panic!("idle control must not queue")
        };
        assert!(registry.0.lock().unwrap().intents.is_empty());
        assert_eq!(registry.0.lock().unwrap().transitions, [transition]);
        assert!(
            driver.boundary.try_lock().is_err(),
            "ordinary work must not enter before the response is observed"
        );
        if matches!(submission.response(), GoalResponse::Resumed(_)) {
            supervisor
                .pause_before_launch(
                    &mut submission,
                    zeroclaw_goal::GoalPauseReason::OperatorPaused,
                )
                .await
                .unwrap();
        }
        drop(submission);
        assert!(driver.boundary.try_lock().is_ok());
        assert!(driver.command.try_lock().is_ok());
    }
}

#[tokio::test]
async fn read_only_commands_never_wait_for_an_ordinary_turn_or_record_an_intent() {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    registry.seed(&ingress.session_key().durable_id(), TaskStatus::Running);
    let supervisor = supervisor(registry.clone());
    let ordinary = driver.boundary.clone().lock_owned().await;
    for command in [GoalCommand::Help, GoalCommand::Status, GoalCommand::Budget] {
        let admission = tokio::time::timeout(
            Duration::from_secs(1),
            supervisor.admit_control(&settings(&config), ingress.clone(), driver.clone(), command),
        )
        .await
        .expect("read-only control must not wait for ordinary completion")
        .unwrap();
        assert!(matches!(admission, GoalControlAdmission::Direct(_)));
        drop(admission);
    }
    assert!(registry.0.lock().unwrap().intents.is_empty());
    assert!(registry.0.lock().unwrap().transitions.is_empty());
    assert!(driver.boundary.try_lock().is_err());
    drop(ordinary);
}

#[test]
fn command_classification_is_total_and_resume_text_never_changes_it() {
    let cases = [
        (
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "Complete the bounded fixture".into(),
            },
            GoalControlClass::Normal(GoalQueuedControl::Start),
        ),
        (
            GoalCommand::Resume { response: None },
            GoalControlClass::Normal(GoalQueuedControl::Resume),
        ),
        (
            GoalCommand::Resume {
                response: Some("Choice A\nKeep the other constraints".into()),
            },
            GoalControlClass::Normal(GoalQueuedControl::Resume),
        ),
        (
            GoalCommand::Pause { immediate: false },
            GoalControlClass::Normal(GoalQueuedControl::Pause),
        ),
        (
            GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
            GoalControlClass::Normal(GoalQueuedControl::Budget),
        ),
        (GoalCommand::Cancel, GoalControlClass::Immediate),
        (
            GoalCommand::Pause { immediate: true },
            GoalControlClass::Immediate,
        ),
        (GoalCommand::Help, GoalControlClass::ReadOnly),
        (GoalCommand::Status, GoalControlClass::ReadOnly),
        (GoalCommand::Budget, GoalControlClass::ReadOnly),
    ];
    for (command, expected) in cases {
        assert_eq!(classify_goal_control(&command), expected, "{command:?}");
    }
}

#[test]
fn queued_controls_have_native_snake_case_tags() {
    for (control, tag) in [
        (GoalQueuedControl::Start, "start"),
        (GoalQueuedControl::Resume, "resume"),
        (GoalQueuedControl::Pause, "pause"),
        (GoalQueuedControl::Budget, "budget"),
    ] {
        let encoded = serde_json::to_value(control).unwrap();
        assert_eq!(encoded, tag);
        assert_eq!(
            serde_json::from_value::<GoalQueuedControl>(encoded).unwrap(),
            control
        );
    }
}

#[tokio::test]
async fn scheduled_ticket_still_observes_its_issuing_checkpoint_closure() {
    let checkpoint = GoalParentTurnCheckpoint::new();
    let mut ticket = checkpoint.register_control().unwrap();
    let worker_checkpoint = checkpoint.clone();
    let worker = zeroclaw_spawn::spawn!(async move { worker_checkpoint.admit_operation().await });
    tokio::time::timeout(Duration::from_secs(1), ticket.wait_until_turn())
        .await
        .unwrap()
        .unwrap();
    checkpoint.close();
    tokio::time::timeout(Duration::from_secs(1), ticket.wait_closed())
        .await
        .expect("closure must wake a ticket already waiting for the command lock");
    drop(ticket);
    assert!(worker.await.unwrap().is_err());
}

struct BoundarylessDriver(GoalSessionKey);

#[async_trait]
impl GoalSessionDriver for BoundarylessDriver {
    fn session_key(&self) -> &GoalSessionKey {
        &self.0
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> Result<GoalSessionLease> {
        Ok(GoalSessionLease::new(
            GoalSessionBinding::new(self.0.clone()),
            (),
        ))
    }

    async fn acquire_execution(
        &self,
        _ingress: &GoalIngressContext,
        _scope: &GoalExecutionScope,
        _checkpoint: &GoalParentTurnCheckpoint,
    ) -> Result<Box<dyn GoalSessionExecutionLease>> {
        bail!("fixture does not launch execution")
    }
}

#[tokio::test]
async fn default_driver_has_no_shared_turn_boundary() {
    let key = GoalSessionKey::zero_code("boundaryless-fixture").unwrap();
    let driver = BoundarylessDriver(key.clone());
    let ingress = GoalIngressContext::trusted(
        key,
        "fixture-agent",
        "fixture-route",
        GoalIngressPrincipal::ZeroCode {
            tui_id: "fixture-client".into(),
        },
    )
    .unwrap();
    let immediate = driver.try_turn_boundary(&ingress).await.unwrap();
    assert!(immediate.is_some());
    drop(immediate);
    let checkpoint = GoalParentTurnCheckpoint::new();
    let boundary = tokio::time::timeout(
        Duration::from_secs(1),
        driver.await_turn_boundary(&ingress, &checkpoint),
    )
    .await
    .expect("boundaryless fake driver must not wait")
    .unwrap();
    drop(boundary);
}

/// The same two-lock shape as an ordinary surface: command serialization is
/// distinct from the foreground turn permit. Tests must not conflate them.
struct SerializedBoundaryDriver {
    base: BoundarylessDriver,
    command: Arc<tokio::sync::Mutex<()>>,
    boundary: Arc<tokio::sync::Mutex<()>>,
    deny_bind: std::sync::atomic::AtomicBool,
    bind_starts: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl GoalSessionDriver for SerializedBoundaryDriver {
    fn session_key(&self) -> &GoalSessionKey {
        self.base.session_key()
    }

    async fn bind(&self, _ingress: &GoalIngressContext) -> Result<GoalSessionLease> {
        self.bind_starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.deny_bind.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("fixture live authority revoked");
        }
        let command = self.command.clone().lock_owned().await;
        if self.deny_bind.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("fixture live authority revoked");
        }
        Ok(GoalSessionLease::new(
            GoalSessionBinding::new(self.base.0.clone()),
            command,
        ))
    }

    async fn try_turn_boundary(
        &self,
        _ingress: &GoalIngressContext,
    ) -> Result<Option<GoalTurnBoundary>> {
        Ok(self
            .boundary
            .clone()
            .try_lock_owned()
            .ok()
            .map(GoalTurnBoundary::new))
    }

    async fn await_turn_boundary(
        &self,
        _ingress: &GoalIngressContext,
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> Result<GoalTurnBoundary> {
        tokio::select! {
            biased;
            () = checkpoint.wait_closed() => bail!("fixture boundary wait closed"),
            guard = self.boundary.clone().lock_owned() => Ok(GoalTurnBoundary::new(guard)),
        }
    }

    async fn acquire_execution(
        &self,
        ingress: &GoalIngressContext,
        scope: &GoalExecutionScope,
        checkpoint: &GoalParentTurnCheckpoint,
    ) -> Result<Box<dyn GoalSessionExecutionLease>> {
        self.base
            .acquire_execution(ingress, scope, checkpoint)
            .await
    }
}

fn serialized_fixture() -> (Arc<SerializedBoundaryDriver>, GoalIngressContext) {
    let key = GoalSessionKey::zero_code("serialized-boundary-fixture").unwrap();
    let ingress = GoalIngressContext::trusted(
        key.clone(),
        "fixture-agent",
        "fixture-route",
        GoalIngressPrincipal::ZeroCode {
            tui_id: "fixture-client".into(),
        },
    )
    .unwrap();
    (
        Arc::new(SerializedBoundaryDriver {
            base: BoundarylessDriver(key),
            command: Arc::new(tokio::sync::Mutex::new(())),
            boundary: Arc::new(tokio::sync::Mutex::new(())),
            deny_bind: std::sync::atomic::AtomicBool::new(false),
            bind_starts: std::sync::atomic::AtomicUsize::new(0),
        }),
        ingress,
    )
}

fn managed_fixture() -> (Arc<SerializedBoundaryDriver>, GoalIngressContext) {
    let (driver, _) = serialized_fixture();
    let ingress = GoalIngressContext::trusted(
        driver.session_key().clone(),
        "fixture-agent",
        "zerocode:serialized-boundary-fixture",
        GoalIngressPrincipal::ZeroCode {
            tui_id: "fixture-client".into(),
        },
    )
    .unwrap();
    (driver, ingress)
}

async fn assert_unlaunched_epoch_paused(registry: &Registry, task_id: &str, epoch: i64) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let settled = {
                let state = registry.0.lock().unwrap();
                let task = state.task.as_ref().unwrap();
                assert_eq!(
                    task.id, task_id,
                    "cleanup must preserve session task identity"
                );
                if task.status == TaskStatus::Paused {
                    assert_eq!(task.execution_epoch, epoch + 1);
                    let goal = state.goal.as_ref().unwrap();
                    assert_eq!(
                        goal.pause_reason,
                        Some(zeroclaw_goal::GoalPauseReason::ExternalDependency)
                    );
                    assert_eq!(state.transitions.last(), Some(&"pause"));
                    true
                } else {
                    assert_eq!(task.status, TaskStatus::Running);
                    false
                }
            };
            if settled {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping a committed launch must not strand Running without a worker");
}

fn launch_command(resume: bool) -> GoalCommand {
    if resume {
        GoalCommand::Resume { response: None }
    } else {
        GoalCommand::Start {
            budget: GoalBudgetSelection::Defaults,
            objective: "Complete the bounded fixture".into(),
        }
    }
}

#[tokio::test]
async fn aborting_committed_launch_audit_cannot_strand_the_epoch() {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    let ordinary = driver.boundary.clone().lock_owned().await;
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress,
                driver.clone(),
                launch_command(false),
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    drop(ordinary);
    let ready = deferred.wait().await.unwrap();
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let wait = Arc::new(tokio::sync::Semaphore::new(0));
    {
        let mut state = registry.0.lock().unwrap();
        state.settlement_entered = Some(entered.clone());
        state.settlement_wait = Some(wait.clone());
    }
    let committing = zeroclaw_spawn::spawn!(async move {
        let config = goal_config();
        ready.commit(&settings(&config)).await
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
    wait.add_permits(1);
    assert!(matches!(
        wait_for_settlement(&registry).await,
        IntentState::Settled(GoalControlIntentSettlement::Abandoned { .. })
    ));
    assert!(driver.command.try_lock().is_ok());
    assert!(driver.boundary.try_lock().is_ok());
}

#[tokio::test]
async fn dropped_launch_preserves_a_competing_terminal_or_pause_winner() {
    let config = goal_config();
    use zeroclaw_goal::{GoalPauseReason, GoalPauseState, GoalTaskRegistry, GoalTransitionResult};
    for status in [
        TaskStatus::Cancelled,
        TaskStatus::Completed,
        TaskStatus::Paused,
    ] {
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        let supervisor = supervisor(registry.clone());
        let GoalControlAdmission::Direct(submission) = supervisor
            .admit_control(&settings(&config), ingress, driver, launch_command(false))
            .await
            .unwrap()
        else {
            panic!("idle launch must apply directly");
        };
        let task = registry.0.lock().unwrap().task.clone().unwrap();
        let session = task.session_id.as_deref().unwrap();
        let result = if status == TaskStatus::Paused {
            registry
                .pause_session_goal(
                    &task.id,
                    session,
                    task.execution_epoch,
                    GoalPauseState {
                        reason: GoalPauseReason::OperatorPaused,
                        description: None,
                        blockers: vec![],
                    },
                )
                .await
        } else {
            registry
                .finish_session_goal(&task.id, session, task.execution_epoch, status, None)
                .await
        };
        assert_eq!(result.unwrap(), GoalTransitionResult::Applied);
        let lookups = registry.0.lock().unwrap().session_lookups;
        drop(submission);
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.0.lock().unwrap().session_lookups == lookups {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cleanup must inspect the exact durable winner");
        let state = registry.0.lock().unwrap();
        let current = state.task.as_ref().unwrap();
        assert_eq!(current.status, status);
        assert_eq!(current.execution_epoch, task.execution_epoch + 1);
        assert_eq!(state.transitions.len(), 2);
        if status == TaskStatus::Paused {
            assert_eq!(
                state.goal.as_ref().unwrap().pause_reason,
                Some(GoalPauseReason::OperatorPaused)
            );
        }
    }
}

#[tokio::test]
async fn reported_launch_bind_failure_stays_terminal_after_submission_drop() {
    let config = goal_config();
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    let GoalControlAdmission::Direct(mut submission) = supervisor
        .admit_control(
            &settings(&config),
            ingress,
            driver.clone(),
            launch_command(false),
        )
        .await
        .unwrap()
    else {
        panic!("idle launch must apply directly");
    };
    driver
        .deny_bind
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let error = supervisor
        .launch_after_response(&settings_source, &mut submission)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("fixture live authority revoked"));
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Failed
    );
    drop(submission);
    assert_eq!(registry.0.lock().unwrap().transitions, ["start", "finish"]);
}

#[tokio::test]
async fn dropping_direct_launch_before_notice_delivery_pauses_the_exact_epoch() {
    let config = goal_config();
    for resume in [false, true] {
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        if resume {
            registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
        }
        let supervisor = supervisor(registry.clone());
        let GoalControlAdmission::Direct(submission) = supervisor
            .admit_control(
                &settings(&config),
                ingress,
                driver.clone(),
                launch_command(resume),
            )
            .await
            .unwrap()
        else {
            panic!("idle launch must apply directly");
        };
        let task = registry.0.lock().unwrap().task.clone().unwrap();
        drop(submission);
        assert_unlaunched_epoch_paused(&registry, &task.id, task.execution_epoch).await;
        assert!(driver.command.try_lock().is_ok());
        assert!(driver.boundary.try_lock().is_ok());
        assert!(registry.0.lock().unwrap().intents.is_empty());
    }
}

#[tokio::test]
async fn dropping_committed_launch_during_notice_delivery_preserves_applied_audit() {
    let config = goal_config();
    for resume in [false, true] {
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        if resume {
            registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
        }
        let supervisor = supervisor(registry.clone());
        let ordinary = driver.boundary.clone().lock_owned().await;
        let deferred = queued(
            supervisor
                .admit_control(
                    &settings(&config),
                    ingress,
                    driver.clone(),
                    launch_command(resume),
                )
                .await
                .unwrap(),
            if resume {
                GoalQueuedControl::Resume
            } else {
                GoalQueuedControl::Start
            },
        );
        drop(ordinary);
        let committed = deferred
            .wait()
            .await
            .unwrap()
            .commit(&settings(&config))
            .await
            .unwrap();
        let task = registry.0.lock().unwrap().task.clone().unwrap();
        drop(committed);
        assert_unlaunched_epoch_paused(&registry, &task.id, task.execution_epoch).await;
        assert_eq!(
            registry.0.lock().unwrap().intents[0].1,
            IntentState::Settled(GoalControlIntentSettlement::Applied)
        );
        assert!(driver.command.try_lock().is_ok());
        assert!(driver.boundary.try_lock().is_ok());
    }
}

#[tokio::test]
async fn aborting_launch_rebind_retains_the_exact_epoch_cleanup_obligation() {
    let config = goal_config();
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
    for resume in [false, true] {
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        if resume {
            registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
        }
        let supervisor = supervisor(registry.clone());
        let ordinary = driver.boundary.clone().lock_owned().await;
        let deferred = queued(
            supervisor
                .admit_control(
                    &settings(&config),
                    ingress,
                    driver.clone(),
                    launch_command(resume),
                )
                .await
                .unwrap(),
            if resume {
                GoalQueuedControl::Resume
            } else {
                GoalQueuedControl::Start
            },
        );
        drop(ordinary);
        let committed = deferred
            .wait()
            .await
            .unwrap()
            .commit(&settings(&config))
            .await
            .unwrap();
        let mut submission = committed.release();
        let command = driver.command.clone().lock_owned().await;
        let task = registry.0.lock().unwrap().task.clone().unwrap();
        // Poll launch once with command serialization occupied so cancellation
        // happens after request extraction, not before the future starts.
        let binds = driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst);
        let mut launch =
            Box::pin(supervisor.launch_after_response(&settings_source, &mut submission));
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(launch.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(
            driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
            binds + 1
        );
        drop(launch);
        drop(command);
        assert_unlaunched_epoch_paused(&registry, &task.id, task.execution_epoch).await;
        assert!(
            supervisor
                .launch_after_response(&settings_source, &mut submission)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            registry.0.lock().unwrap().transitions.last(),
            Some(&"pause")
        );
        drop(submission);
        assert!(driver.command.try_lock().is_ok());
        assert!(driver.boundary.try_lock().is_ok());
    }
}

#[tokio::test]
async fn failed_intent_record_returns_no_ack_and_commits_nothing() {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    registry.0.lock().unwrap().fail_record = true;
    let supervisor = supervisor(registry.clone());
    let ordinary = driver.boundary.clone().lock_owned().await;
    let result = supervisor
        .admit_control(
            &settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "Complete fixture".into(),
            },
        )
        .await;
    assert!(format!("{:#}", result.unwrap_err()).contains("fixture intent record unavailable"));
    assert!(registry.0.lock().unwrap().task.is_none());
    assert!(registry.0.lock().unwrap().intents.is_empty());
    assert!(driver.command.try_lock().is_ok());
    drop(ordinary);
}

#[tokio::test]
async fn revoked_live_bind_preserves_the_error_without_disclosing_task_state() {
    let (driver, _, registry, _, ordinary, deferred) = queued_pause_fixture().await;
    driver
        .deny_bind
        .store(true, std::sync::atomic::Ordering::SeqCst);
    drop(ordinary);
    let closed = deferred.wait().await.unwrap_err();
    assert_eq!(closed.response(), &GoalResponse::Stale);
    assert!(format!("{:#}", anyhow::Error::new(closed)).contains("fixture live authority revoked"));
    assert!(registry.0.lock().unwrap().transitions.is_empty());
    assert!(matches!(
        wait_for_settlement(&registry).await,
        IntentState::Settled(GoalControlIntentSettlement::Abandoned { .. })
    ));
}

#[tokio::test]
async fn restart_gate_closes_queued_start_and_refuses_new_registration() {
    let live = LiveConfig::new(managed_config());
    let settings_source = source(&live);
    let snapshot = settings_source.snapshot();
    let settings = snapshot.settings();
    let (driver, ingress) = managed_fixture();
    let registry = Arc::new(Registry::default());
    let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
    let runtime = GoalRuntime::new(registry.clone());
    let supervisor = coordinator
        .new_supervisor(
            Arc::new(runtime.execution_engine(Arc::new(NoModelAccounting))),
            settings_source,
        )
        .await;
    let ordinary = driver.boundary.clone().lock_owned().await;
    let start = GoalCommand::Start {
        budget: GoalBudgetSelection::Defaults,
        objective: "Complete fixture".into(),
    };
    let deferred = queued(
        supervisor
            .admit_control(&settings, ingress.clone(), driver.clone(), start.clone())
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    let (retired, restarted) = tokio::join!(
        retire_without_waiting(&supervisor),
        tokio::time::timeout(Duration::from_secs(1), coordinator.quiesce_for_restart()),
    );
    assert!(!retired, "the queued control still holds custody");
    restarted.unwrap().unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), deferred.wait())
            .await
            .unwrap()
            .is_err()
    );
    let refused = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(&settings, ingress, driver, start),
    )
    .await
    .unwrap();
    assert!(refused.is_err());
    assert_eq!(registry.0.lock().unwrap().intents.len(), 1);
    assert!(registry.0.lock().unwrap().task.is_none());
    assert!(retire_without_waiting(&supervisor).await);
    drop(ordinary);
}

#[tokio::test]
async fn canonical_snapshot_fences_actual_admit_control_before_side_effects() {
    const RETRY: &str =
        "Goal policy changed while this command was being admitted; retry the command";

    for (resume, invalid) in [false, true].into_iter().flat_map(|resume| {
        [
            "stale",
            "wrong-source",
            "unstamped",
            "disabled",
            "agent-disabled",
        ]
        .map(|invalid| (resume, invalid))
    }) {
        let (driver, ingress) = managed_fixture();
        let registry = Arc::new(Registry::default());
        if resume {
            registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
        }
        let live = LiveConfig::new(managed_config());
        let settings_source = source(&live);
        let private = LiveConfig::new(live.snapshot());
        let snapshot = if invalid == "wrong-source" {
            source(&private).snapshot()
        } else {
            settings_source.snapshot()
        };
        if invalid == "stale" {
            live.publish(live.next_revision().unwrap(), live.snapshot())
                .unwrap();
        }
        let snapshot = if matches!(invalid, "disabled" | "agent-disabled") {
            let mut config = live.snapshot();
            if invalid == "disabled" {
                config.goal.enabled = false;
            } else {
                config.agents.get_mut("fixture-agent").unwrap().enabled = false;
            }
            live.publish(live.next_revision().unwrap(), config).unwrap();
            settings_source.snapshot()
        } else {
            snapshot
        };
        let config = live.snapshot();
        let settings = if invalid == "unstamped" {
            settings(&config)
        } else {
            snapshot.settings()
        };
        let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
        let runtime = GoalRuntime::new(registry.clone());
        let supervisor = coordinator
            .new_supervisor(
                Arc::new(runtime.execution_engine(Arc::new(NoModelAccounting))),
                settings_source,
            )
            .await;
        let before = {
            let state = registry.0.lock().unwrap();
            serde_json::to_value((&state.task, &state.goal)).unwrap()
        };

        let result = supervisor
            .admit_control(&settings, ingress, driver.clone(), launch_command(resume))
            .await;
        if matches!(invalid, "disabled" | "agent-disabled") {
            let GoalControlAdmission::Direct(submission) = result.unwrap() else {
                panic!("disabled scope cannot queue a control");
            };
            assert_eq!(submission.response(), &GoalResponse::Disabled);
        } else {
            let error = result.unwrap_err();
            if invalid == "stale" {
                assert_eq!(error.to_string(), RETRY);
            }
        }
        let state = registry.0.lock().unwrap();
        assert_eq!(
            serde_json::to_value((&state.task, &state.goal)).unwrap(),
            before
        );
        assert!(state.intents.is_empty());
        assert!(state.transitions.is_empty());
        assert_eq!(state.session_lookups, 0);
        assert_eq!(
            driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    for resume in [false, true] {
        let (driver, ingress) = managed_fixture();
        let registry = Arc::new(Registry::default());
        if resume {
            registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
        }
        let live = LiveConfig::new(managed_config());
        let settings_source = source(&live);
        let snapshot = settings_source.snapshot();
        let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
        let runtime = GoalRuntime::new(registry.clone());
        let supervisor = coordinator
            .new_supervisor(
                Arc::new(runtime.execution_engine(Arc::new(NoModelAccounting))),
                settings_source,
            )
            .await;
        let settings = snapshot.settings();

        let GoalControlAdmission::Direct(mut submission) = supervisor
            .admit_control(&settings, ingress, driver, launch_command(resume))
            .await
            .unwrap()
        else {
            panic!("an idle Start or Resume must apply directly");
        };
        if resume {
            assert!(matches!(submission.response(), GoalResponse::Resumed(_)));
        } else {
            assert!(matches!(submission.response(), GoalResponse::Started(_)));
        }
        {
            let state = registry.0.lock().unwrap();
            assert!(state.intents.is_empty());
            assert_eq!(state.transitions, [if resume { "resume" } else { "start" }]);
        }
        supervisor
            .pause_before_launch(
                &mut submission,
                zeroclaw_goal::GoalPauseReason::OperatorPaused,
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn managed_disabled_scope_returns_disabled_for_every_public_goal_command() {
    for agent_disabled in [false, true] {
        let mut config = managed_config();
        if agent_disabled {
            config.agents.get_mut("fixture-agent").unwrap().enabled = false;
        } else {
            config.goal.enabled = false;
        }
        let live = LiveConfig::new(config);
        let settings_source = source(&live);
        let snapshot = settings_source.snapshot();
        let (driver, ingress) = managed_fixture();
        let registry = Arc::new(Registry::default());
        let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
        let supervisor = coordinator
            .new_supervisor(
                Arc::new(
                    GoalRuntime::new(registry.clone())
                        .execution_engine(Arc::new(NoModelAccounting)),
                ),
                settings_source,
            )
            .await;
        for command in [
            launch_command(false),
            launch_command(true),
            GoalCommand::Status,
            GoalCommand::Budget,
            GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
            GoalCommand::Pause { immediate: false },
            GoalCommand::Pause { immediate: true },
            GoalCommand::Cancel,
            GoalCommand::Help,
        ] {
            let GoalControlAdmission::Direct(submission) = supervisor
                .admit_control(
                    &snapshot.settings(),
                    ingress.clone(),
                    driver.clone(),
                    command,
                )
                .await
                .unwrap()
            else {
                panic!("disabled scope cannot queue a control");
            };
            assert_eq!(submission.response(), &GoalResponse::Disabled);
        }
        let state = registry.0.lock().unwrap();
        assert!(state.task.is_none());
        assert!(state.intents.is_empty());
        assert!(state.transitions.is_empty());
        assert_eq!(state.session_lookups, 0);
        assert_eq!(
            driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}

#[tokio::test]
async fn managed_deferred_apply_revalidates_canonical_snapshot_after_wait() {
    for resume in [false, true] {
        for validity in ["stale", "wrong-source", "disabled", "fresh"] {
            let live = LiveConfig::new(managed_config());
            let settings_source = source(&live);
            let (driver, ingress) = managed_fixture();
            let registry = Arc::new(Registry::default());
            if resume {
                registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
            }
            let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
            let supervisor = coordinator
                .new_supervisor(
                    Arc::new(
                        GoalRuntime::new(registry.clone())
                            .execution_engine(Arc::new(NoModelAccounting)),
                    ),
                    settings_source.clone(),
                )
                .await;
            let ordinary = driver.boundary.clone().lock_owned().await;
            let initial = settings_source.snapshot();
            let deferred = queued(
                supervisor
                    .admit_control(
                        &initial.settings(),
                        ingress,
                        driver.clone(),
                        launch_command(resume),
                    )
                    .await
                    .unwrap(),
                if resume {
                    GoalQueuedControl::Resume
                } else {
                    GoalQueuedControl::Start
                },
            );
            drop(ordinary);
            let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
                .await
                .unwrap()
                .unwrap();
            let before = {
                let state = registry.0.lock().unwrap();
                serde_json::to_value((&state.task, &state.goal)).unwrap()
            };
            let private = LiveConfig::new(live.snapshot());
            let captured = if validity == "wrong-source" {
                source(&private).snapshot()
            } else {
                settings_source.snapshot()
            };
            let mut next = live.snapshot();
            next.goal.default_token_limit = Some(2000);
            if validity == "disabled" {
                next.goal.enabled = false;
            }
            live.publish(live.next_revision().unwrap(), next).unwrap();
            let captured = if matches!(validity, "fresh" | "disabled") {
                settings_source.snapshot()
            } else {
                captured
            };
            if validity == "disabled" {
                // A disabled control must not consult even the claim predicate.
                registry.0.lock().unwrap().reject_claim = true;
            }
            let result = ready.commit(&captured.settings()).await;
            if validity == "fresh" {
                let committed = result.unwrap();
                assert!(matches!(
                    committed.response(),
                    GoalResponse::Started(_) | GoalResponse::Resumed(_)
                ));
                assert_eq!(
                    registry.0.lock().unwrap().transitions,
                    [if resume { "resume" } else { "start" }]
                );
                if !resume {
                    assert_eq!(
                        registry
                            .0
                            .lock()
                            .unwrap()
                            .goal
                            .as_ref()
                            .unwrap()
                            .effective_token_limit,
                        Some(2000)
                    );
                }
                let mut submission = committed.release();
                supervisor
                    .pause_before_launch(
                        &mut submission,
                        zeroclaw_goal::GoalPauseReason::OperatorPaused,
                    )
                    .await
                    .unwrap();
            } else if validity == "disabled" {
                let committed = result.unwrap();
                assert_eq!(committed.response(), &GoalResponse::Disabled);
                drop(committed.release());
                assert_eq!(
                    wait_for_settlement(&registry).await,
                    IntentState::Settled(GoalControlIntentSettlement::Superseded {
                        reason: "scope_disabled".into(),
                    })
                );
                let state = registry.0.lock().unwrap();
                assert_eq!(
                    serde_json::to_value((&state.task, &state.goal)).unwrap(),
                    before
                );
                assert!(state.transitions.is_empty());
            } else {
                assert!(result.is_err());
                assert!(matches!(
                    wait_for_settlement(&registry).await,
                    IntentState::Settled(GoalControlIntentSettlement::Abandoned { .. })
                ));
                let state = registry.0.lock().unwrap();
                assert_eq!(
                    serde_json::to_value((&state.task, &state.goal)).unwrap(),
                    before
                );
                assert!(state.transitions.is_empty());
            }
            assert!(driver.command.try_lock().is_ok());
            assert!(driver.boundary.try_lock().is_ok());
        }
    }
}

#[tokio::test]
async fn managed_admission_rechecks_snapshot_after_command_bind_wait() {
    let live = LiveConfig::new(managed_config());
    let settings_source = source(&live);
    let (driver, ingress) = managed_fixture();
    let registry = Arc::new(Registry::default());
    let coordinator = zeroclaw_goal::GoalExecutionRestartCoordinator::new();
    let supervisor = coordinator
        .new_supervisor(
            Arc::new(
                GoalRuntime::new(registry.clone()).execution_engine(Arc::new(NoModelAccounting)),
            ),
            settings_source.clone(),
        )
        .await;
    let command = driver.command.clone().lock_owned().await;
    let binding_driver = driver.clone();
    let admitted = zeroclaw_spawn::spawn!(async move {
        let snapshot = settings_source.snapshot();
        supervisor
            .admit_control(
                &snapshot.settings(),
                ingress,
                binding_driver,
                launch_command(false),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while driver.bind_starts.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    live.publish(live.next_revision().unwrap(), live.snapshot())
        .unwrap();
    drop(command);
    let result = tokio::time::timeout(Duration::from_secs(1), admitted)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    let state = registry.0.lock().unwrap();
    assert!(state.task.is_none());
    assert!(state.intents.is_empty());
    assert!(state.transitions.is_empty());
    assert_eq!(state.session_lookups, 0);
    assert!(driver.command.try_lock().is_ok());
}

#[tokio::test]
async fn failed_audit_settlement_does_not_strand_a_new_epoch_without_a_worker() {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    let ordinary = driver.boundary.clone().lock_owned().await;
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress,
                driver,
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Defaults,
                    objective: "Complete fixture".into(),
                },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Start,
    );
    drop(ordinary);
    let ready = deferred.wait().await.unwrap();
    registry.0.lock().unwrap().fail_settlement = true;
    let result = ready.commit(&settings(&config)).await;
    assert!(result.is_err());
    assert_eq!(
        registry.0.lock().unwrap().task.as_ref().unwrap().status,
        TaskStatus::Failed,
        "audit failure after creation must reconcile an unlaunched epoch"
    );
    assert_eq!(registry.0.lock().unwrap().transitions, ["start", "finish"]);
    registry.0.lock().unwrap().fail_settlement = false;
    wait_for_settlement(&registry).await;
}

#[tokio::test]
async fn failed_delivery_abandons_the_control_without_mutating_lifecycle() {
    let (driver, _, registry, _, ordinary, deferred) = queued_pause_fixture().await;
    deferred
        .abandon(zeroclaw_goal::GoalControlAbandonReason::DeliveryFailed)
        .await
        .unwrap();
    assert_eq!(
        wait_for_settlement(&registry).await,
        IntentState::Settled(GoalControlIntentSettlement::Abandoned {
            reason: "delivery_failed".into()
        })
    );
    assert!(registry.0.lock().unwrap().transitions.is_empty());
    assert!(driver.command.try_lock().is_ok());
    assert!(driver.boundary.try_lock().is_err());
    drop(ordinary);
}

#[tokio::test]
async fn connection_task_abort_releases_wait_and_records_its_actual_reason() {
    let (driver, _, registry, _, ordinary, deferred) = queued_pause_fixture().await;
    let deferred =
        deferred.with_abandon_reason(zeroclaw_goal::GoalControlAbandonReason::ConnectionClosed);
    let waiting = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
    tokio::task::yield_now().await;
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    assert_eq!(
        wait_for_settlement(&registry).await,
        IntentState::Settled(GoalControlIntentSettlement::Abandoned {
            reason: "connection_closed".into()
        })
    );
    assert!(registry.0.lock().unwrap().transitions.is_empty());
    assert!(driver.command.try_lock().is_ok());
    assert!(driver.boundary.try_lock().is_err());
    drop(ordinary);
}

#[tokio::test]
async fn opaque_boundary_retains_the_ordinary_turn_permit_until_drop() {
    let (driver, ingress) = serialized_fixture();
    let boundary = driver.try_turn_boundary(&ingress).await.unwrap().unwrap();
    assert!(driver.boundary.try_lock().is_err());
    assert!(driver.try_turn_boundary(&ingress).await.unwrap().is_none());
    // An ordinary boundary must not also reserve Goal command serialization.
    assert!(driver.command.try_lock().is_ok());
    drop(boundary);
    assert!(driver.boundary.try_lock().is_ok());
}

#[tokio::test]
async fn closing_a_wait_does_not_release_an_admitted_ordinary_turn() {
    let (driver, ingress) = serialized_fixture();
    let ordinary = driver.boundary.clone().lock_owned().await;
    let checkpoint = GoalParentTurnCheckpoint::new();
    let waiting_driver = driver.clone();
    let waiting_checkpoint = checkpoint.clone();
    let waiter = zeroclaw_spawn::spawn!(async move {
        waiting_driver
            .await_turn_boundary(&ingress, &waiting_checkpoint)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    checkpoint.close();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("closed admission must withdraw without waiting for an ordinary turn")
            .unwrap()
            .is_err()
    );
    assert!(driver.boundary.try_lock().is_err());
    drop(ordinary);
    assert!(driver.boundary.try_lock().is_ok());
}

#[tokio::test]
async fn each_normal_control_waits_for_the_ordinary_turn_and_retains_commit_guards() {
    let config = goal_config();
    let cases = [
        (
            None,
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "Complete the bounded fixture".into(),
            },
            GoalQueuedControl::Start,
            "start",
        ),
        (
            Some(TaskStatus::Paused),
            GoalCommand::Resume {
                response: Some("Choice A\nKeep the other constraints".into()),
            },
            GoalQueuedControl::Resume,
            "resume",
        ),
        (
            Some(TaskStatus::Running),
            GoalCommand::Pause { immediate: false },
            GoalQueuedControl::Pause,
            "pause",
        ),
        (
            Some(TaskStatus::Running),
            GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
            GoalQueuedControl::Budget,
            "budget",
        ),
    ];
    for (status, command, expected, transition) in cases {
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        if let Some(status) = status {
            registry.seed(&ingress.session_key().durable_id(), status);
        }
        let supervisor = supervisor(registry.clone());
        let ordinary = driver.boundary.clone().lock_owned().await;
        let admission = supervisor
            .admit_control(&settings(&config), ingress.clone(), driver.clone(), command)
            .await
            .unwrap();
        let deferred = queued(admission, expected);
        {
            let state = registry.0.lock().unwrap();
            assert!(state.transitions.is_empty());
            assert_eq!(state.intents.len(), 1);
            assert_eq!(state.intents[0].1, IntentState::Pending);
            let audit = format!("{:?}", state.intents[0].0);
            assert!(!audit.contains("Complete the bounded fixture"));
            assert!(!audit.contains("Keep the other constraints"));
        }
        assert!(
            driver.command.try_lock().is_ok(),
            "queued ack must not retain command lock"
        );
        let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
        let waiting = zeroclaw_spawn::spawn!(async move {
            let ready = deferred
                .wait()
                .await
                .expect("ordinary boundary should reopen");
            assert!(ready_tx.send(ready).is_ok());
        });
        tokio::task::yield_now().await;
        assert!(ready_rx.try_recv().is_err());
        assert!(registry.0.lock().unwrap().transitions.is_empty());
        drop(ordinary);
        let ready = tokio::time::timeout(Duration::from_secs(1), ready_rx)
            .await
            .unwrap()
            .unwrap();
        waiting.await.unwrap();
        assert!(driver.command.try_lock().is_err());
        assert!(driver.boundary.try_lock().is_err());
        assert!(registry.0.lock().unwrap().transitions.is_empty());
        let committed =
            tokio::time::timeout(Duration::from_secs(1), ready.commit(&settings(&config)))
                .await
                .expect("commit must reuse the held command lease")
                .unwrap();
        assert!(driver.command.try_lock().is_err());
        assert!(driver.boundary.try_lock().is_err());
        {
            let state = registry.0.lock().unwrap();
            assert_eq!(state.transitions, [transition]);
            assert_eq!(
                state.intents[0].1,
                IntentState::Settled(GoalControlIntentSettlement::Applied)
            );
        }
        let mut submission = committed.release();
        assert!(driver.command.try_lock().is_ok());
        assert!(driver.boundary.try_lock().is_ok());
        if matches!(
            submission.response(),
            GoalResponse::Started(_) | GoalResponse::Resumed(_)
        ) {
            supervisor
                .pause_before_launch(
                    &mut submission,
                    zeroclaw_goal::GoalPauseReason::OperatorPaused,
                )
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn idle_start_holds_boundary_through_the_observable_response() {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    let supervisor = supervisor(registry.clone());
    let admission = supervisor
        .admit_control(
            &settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "Complete the bounded fixture".into(),
            },
        )
        .await
        .unwrap();
    let GoalControlAdmission::Direct(mut submission) = admission else {
        panic!("idle command must answer directly")
    };
    assert!(matches!(submission.response(), GoalResponse::Started(_)));
    assert!(registry.0.lock().unwrap().intents.is_empty());
    assert!(
        driver.boundary.try_lock().is_err(),
        "ordinary turns cannot enter before response publication"
    );
    supervisor
        .pause_before_launch(
            &mut submission,
            zeroclaw_goal::GoalPauseReason::OperatorPaused,
        )
        .await
        .unwrap();
    drop(submission);
    assert!(driver.boundary.try_lock().is_ok());
}

#[tokio::test]
async fn rejected_controls_record_no_intent_even_when_an_ordinary_turn_is_active() {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    registry.seed(&ingress.session_key().durable_id(), TaskStatus::Running);
    let supervisor = supervisor(registry.clone());
    let ordinary = driver.boundary.clone().lock_owned().await;
    for command in [
        GoalCommand::Start {
            budget: GoalBudgetSelection::Defaults,
            objective: "Different goal".into(),
        },
        GoalCommand::Resume { response: None },
    ] {
        let GoalControlAdmission::Direct(submission) = supervisor
            .admit_control(&settings(&config), ingress.clone(), driver.clone(), command)
            .await
            .unwrap()
        else {
            panic!("already-active intake must not queue")
        };
        assert_eq!(submission.response(), &GoalResponse::AlreadyActive);
        drop(submission);
    }
    assert!(registry.0.lock().unwrap().intents.is_empty());
    assert!(registry.0.lock().unwrap().transitions.is_empty());
    drop(ordinary);
}

async fn wait_for_settlement(registry: &Registry) -> IntentState {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let state = registry.0.lock().unwrap().intents[0].1.clone();
            if matches!(state, IntentState::Settled(_)) {
                return state;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("every deferred owner exit must settle its audit intent")
}

async fn queued_pause_fixture() -> (
    Arc<SerializedBoundaryDriver>,
    GoalIngressContext,
    Arc<Registry>,
    Arc<GoalExecutionSupervisor>,
    tokio::sync::OwnedMutexGuard<()>,
    GoalDeferredControl,
) {
    let config = goal_config();
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    registry.seed(&ingress.session_key().durable_id(), TaskStatus::Running);
    let supervisor = supervisor(registry.clone());
    let ordinary = driver.boundary.clone().lock_owned().await;
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
    (driver, ingress, registry, supervisor, ordinary, deferred)
}

#[tokio::test]
async fn stale_claim_never_runs_a_controller_transition() {
    let config = goal_config();
    let (_, _, registry, _, ordinary, deferred) = queued_pause_fixture().await;
    registry.0.lock().unwrap().reject_claim = true;
    drop(ordinary);
    let ready = tokio::time::timeout(Duration::from_secs(1), deferred.wait())
        .await
        .unwrap()
        .unwrap();
    let committed = ready.commit(&settings(&config)).await.unwrap();
    assert_eq!(committed.response(), &GoalResponse::Stale);
    drop(committed.release());
    assert!(registry.0.lock().unwrap().transitions.is_empty());
    assert!(matches!(
        wait_for_settlement(&registry).await,
        IntentState::Settled(_)
    ));
}

#[tokio::test]
async fn competing_epoch_change_supersedes_the_exact_bound_control() {
    let config = goal_config();
    let (_, _, registry, _, ordinary, deferred) = queued_pause_fixture().await;
    drop(ordinary);
    let ready = deferred.wait().await.unwrap();
    {
        let mut state = registry.0.lock().unwrap();
        let task = state.task.as_mut().unwrap();
        task.execution_epoch += 1;
        task.status = TaskStatus::Paused;
    }
    let committed = ready.commit(&settings(&config)).await.unwrap();
    assert!(matches!(
        committed.response(),
        GoalResponse::AlreadyPaused(_)
    ));
    drop(committed.release());
    let state = registry.0.lock().unwrap();
    assert!(state.transitions.is_empty());
    assert!(matches!(
        state.intents[0].1,
        IntentState::Settled(GoalControlIntentSettlement::Superseded { .. })
    ));
}

#[tokio::test]
async fn dropped_deferred_control_abandons_the_intent_without_touching_an_ordinary_turn() {
    let (driver, _, registry, _, ordinary, deferred) = queued_pause_fixture().await;
    drop(deferred);
    assert!(matches!(
        wait_for_settlement(&registry).await,
        IntentState::Settled(GoalControlIntentSettlement::Abandoned { .. })
    ));
    assert!(driver.boundary.try_lock().is_err());
    assert!(driver.command.try_lock().is_ok());
    assert!(registry.0.lock().unwrap().transitions.is_empty());
    drop(ordinary);
}

#[tokio::test]
async fn disposal_and_restart_close_pending_boundary_waits_without_waiting_for_ordinary_work() {
    let config = goal_config();
    for restart in [false, true] {
        let (driver, ingress) = serialized_fixture();
        let registry = Arc::new(Registry::default());
        let supervisor = supervisor(registry.clone());
        let ordinary = driver.boundary.clone().lock_owned().await;
        let deferred = queued(
            supervisor
                .admit_control(
                    &settings(&config),
                    ingress.clone(),
                    driver.clone(),
                    GoalCommand::Start {
                        budget: GoalBudgetSelection::Defaults,
                        objective: "Complete fixture".into(),
                    },
                )
                .await
                .unwrap(),
            GoalQueuedControl::Start,
        );
        let waiter = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
        tokio::task::yield_now().await;
        if restart {
            tokio::time::timeout(Duration::from_secs(1), supervisor.pause_for_restart())
                .await
                .unwrap()
                .unwrap();
        } else {
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                supervisor.dispose_session(&ingress.session_key().durable_id()),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(result, zeroclaw_goal::GoalTransitionResult::Missing);
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("lifecycle closure must withdraw the pending boundary wait")
                .unwrap()
                .is_err()
        );
        assert!(driver.boundary.try_lock().is_err());
        assert!(matches!(
            wait_for_settlement(&registry).await,
            IntentState::Settled(_)
        ));
        assert!(registry.0.lock().unwrap().transitions.is_empty());
        drop(ordinary);
    }
}

#[tokio::test]
async fn immediate_cancel_overtakes_a_queued_resume_and_preserves_the_terminal_winner() {
    let config = goal_config();
    let live = LiveConfig::new(config.clone());
    let settings_source = source(&live);
    let (driver, ingress) = serialized_fixture();
    let registry = Arc::new(Registry::default());
    registry.seed(&ingress.session_key().durable_id(), TaskStatus::Paused);
    let supervisor = supervisor(registry.clone());
    let ordinary = driver.boundary.clone().lock_owned().await;
    let deferred = queued(
        supervisor
            .admit_control(
                &settings(&config),
                ingress.clone(),
                driver.clone(),
                GoalCommand::Resume {
                    response: Some("Choice A".into()),
                },
            )
            .await
            .unwrap(),
        GoalQueuedControl::Resume,
    );
    let waiter = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
    tokio::task::yield_now().await;
    let admission = tokio::time::timeout(
        Duration::from_secs(1),
        supervisor.admit_control(
            &settings(&config),
            ingress,
            driver.clone(),
            GoalCommand::Cancel,
        ),
    )
    .await
    .expect("immediate control must not wait for the ordinary turn")
    .unwrap();
    let GoalControlAdmission::Direct(mut cancelled) = admission else {
        panic!("cancel must never queue")
    };
    assert!(matches!(cancelled.response(), GoalResponse::Cancelled(_)));
    supervisor
        .launch_after_response(&settings_source, &mut cancelled)
        .await
        .unwrap();
    drop(cancelled);
    drop(ordinary);
    let ready = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let committed = ready.commit(&settings(&config)).await.unwrap();
    assert!(matches!(committed.response(), GoalResponse::Terminal(_)));
    drop(committed.release());
    let state = registry.0.lock().unwrap();
    assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Cancelled);
    assert_eq!(state.transitions, ["finish"]);
    assert!(matches!(
        state.intents[0].1,
        IntentState::Settled(GoalControlIntentSettlement::Superseded { .. })
    ));
}

#[tokio::test]
async fn lifecycle_command_then_boundary_lock_does_not_deadlock_a_deferred_waiter() {
    let config = goal_config();
    let (driver, _, registry, _, ordinary, deferred) = queued_pause_fixture().await;
    let waiter = zeroclaw_spawn::spawn!(async move { deferred.wait().await });
    tokio::task::yield_now().await;
    let lifecycle_driver = driver.clone();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let lifecycle = zeroclaw_spawn::spawn!(async move {
        let command = lifecycle_driver.command.clone().lock_owned().await;
        held_tx.send(()).unwrap();
        let boundary = lifecycle_driver.boundary.clone().lock_owned().await;
        drop(boundary);
        drop(command);
    });
    tokio::time::timeout(Duration::from_secs(1), held_rx)
        .await
        .unwrap()
        .unwrap();
    drop(ordinary);
    tokio::time::timeout(Duration::from_secs(1), lifecycle)
        .await
        .expect("waiter must release the boundary before waiting on the command lock")
        .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let committed = ready.commit(&settings(&config)).await.unwrap();
    drop(committed.release());
    assert_eq!(registry.0.lock().unwrap().transitions, ["pause"]);
}
