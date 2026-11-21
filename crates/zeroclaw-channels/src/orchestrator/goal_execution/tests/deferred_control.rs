use super::*;

#[derive(Default)]
struct LaunchCallProbe(std::sync::atomic::AtomicUsize);

impl zeroclaw_api::attribution::Attributable for LaunchCallProbe {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }

    fn alias(&self) -> &str {
        "launch-call-probe"
    }
}

#[async_trait]
impl ModelProvider for LaunchCallProbe {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> Result<String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        bail!("unexpected model call after revocation")
    }

    async fn chat(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        bail!("unexpected model call after revocation")
    }
}

#[tokio::test]
async fn matrix_launch_handoff_rechecks_live_revocation() {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_runtime::control_plane::ControlPlaneHandle;

    let directory = tempfile::tempdir().unwrap();
    let control_plane = ControlPlaneHandle::open(directory.path()).unwrap();
    let store = control_plane.goal_store().unwrap();
    let provider = Arc::new(LaunchCallProbe::default());
    let channel = Arc::new(MatrixGoalErrorRecordingChannel::default());
    let mut context = super::super::super::test_channel_ctx_with_backend_channel_and_provider(
        Arc::new(FailingAppendSessionBackend),
        channel,
        provider.clone(),
        0,
    );
    let mut config = fixture_goal_config(&context);
    config.data_dir = directory.path().to_owned();
    let live = zeroclaw_config::live::LiveConfig::new(config.clone());
    Arc::get_mut(&mut context).unwrap().live_config = live.handle();
    let key = "matrix_live_launch_fixture";
    super::super::super::foreground::install_goal_test_control_plane(
        &context.persist_locks,
        key,
        control_plane,
    );
    let message = ChannelMessage::new(
        "event",
        "fixture-user",
        "room",
        "/goal start fixture",
        "matrix",
        1,
    );
    let mut command = submit_matrix_goal(
        context,
        key.into(),
        message,
        GoalCommand::Start {
            budget: GoalBudgetSelection::Defaults,
            objective: "fixture".into(),
        },
    )
    .await
    .unwrap();
    let GoalResponse::Started(projection) = command.response() else {
        panic!("fixture must start before revocation");
    };
    let scope =
        GoalExecutionScope::new(&projection.task_id, key, projection.execution_epoch).unwrap();
    config.goal.enabled = false;
    live.publish(live.next_revision().unwrap(), config).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), command.after_response())
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            command.supervisor.drain(&scope)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(provider.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    let task = store.current_goal_for_session(key).await.unwrap().unwrap();
    assert_ne!(
        task.status,
        zeroclaw_runtime::control_plane::TaskStatus::Running
    );
}

#[tokio::test]
async fn matrix_initial_admission_uses_live_policy_and_budget_presence() {
    use zeroclaw_commands::goal::{GoalBudgetLimits, GoalBudgetSelection, GoalCommand};
    use zeroclaw_runtime::control_plane::ControlPlaneHandle;

    for case in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let control_plane = ControlPlaneHandle::open(directory.path()).unwrap();
        let store = control_plane.goal_store().unwrap();
        let mut context = super::super::super::test_channel_ctx_with_backend(Arc::new(
            FailingAppendSessionBackend,
        ));
        let mut config = fixture_goal_config(&context);
        config.data_dir = directory.path().to_owned();
        let live = zeroclaw_config::live::LiveConfig::new(config.clone());
        Arc::get_mut(&mut context).unwrap().live_config = live.handle();
        let key = "matrix_live_initial_fixture";
        super::super::super::foreground::install_goal_test_control_plane(
            &context.persist_locks,
            key,
            control_plane,
        );
        config.goal.default_token_limit = None;
        config.goal.default_cost_limit_usd = None;
        if case == 1 {
            config.goal.enabled = false;
        }
        if case == 4 {
            config.goal.default_token_limit = Some(0);
            config.goal.default_cost_limit_usd = Some(0.0);
        }
        live.publish(live.next_revision().unwrap(), config).unwrap();
        let budget = match case {
            2 => GoalBudgetSelection::Limits(GoalBudgetLimits {
                token_limit: Some(1000),
                cost_limit_usd: None,
            }),
            3 => GoalBudgetSelection::Limits(GoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: Some(1.0),
            }),
            _ => GoalBudgetSelection::Defaults,
        };
        let message = ChannelMessage::new(
            "event",
            "fixture-user",
            "room",
            "/goal start fixture",
            "matrix",
            1,
        );
        let command = submit_matrix_goal(
            context,
            key.into(),
            message,
            GoalCommand::Start {
                budget,
                objective: "fixture".into(),
            },
        )
        .await
        .unwrap();
        match case {
            0 => assert!(matches!(command.response(), GoalResponse::BudgetRequired)),
            1 => assert!(matches!(command.response(), GoalResponse::Disabled)),
            _ => assert!(matches!(command.response(), GoalResponse::Started(_))),
        }
        if case < 2 {
            assert!(store.current_goal_for_session(key).await.unwrap().is_none());
        }
        command.response_delivery_failed().await.unwrap();
    }
}

#[tokio::test]
async fn matrix_queued_start_uses_live_defaults_and_revocation_at_commit() {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_runtime::control_plane::ControlPlaneHandle;

    for disable in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let control_plane = ControlPlaneHandle::open(directory.path()).unwrap();
        let store = control_plane.goal_store().unwrap();
        let mut context = super::super::super::test_channel_ctx_with_backend(Arc::new(
            FailingAppendSessionBackend,
        ));
        let mut config = fixture_goal_config(&context);
        config.data_dir = directory.path().to_owned();
        let live = zeroclaw_config::live::LiveConfig::new(config.clone());
        Arc::get_mut(&mut context).unwrap().live_config = live.handle();
        let key = "matrix_live_deferred_fixture";
        super::super::super::foreground::install_goal_test_control_plane(
            &context.persist_locks,
            key,
            control_plane,
        );
        let foreground = foreground_lock(&context.persist_locks, key);
        let ordinary = foreground.clone().lock_owned().await;
        let message = ChannelMessage::new(
            "event",
            "fixture-user",
            "room",
            "/goal start fixture",
            "matrix",
            1,
        );
        let command = submit_matrix_goal(
            context,
            key.into(),
            message.clone(),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "fixture".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(command.response(), GoalResponse::Queued(_)));
        let channel = Arc::new(MatrixGoalErrorRecordingChannel::default());
        let output = channel.clone();
        let delivery = zeroclaw_spawn::spawn!(async move {
            deliver_matrix_goal_command(command, Some(output), message).await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while channel.messages.lock().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        config.goal.enabled = !disable;
        config.goal.default_token_limit = None;
        config.goal.default_cost_limit_usd = None;
        live.publish(live.next_revision().unwrap(), config).unwrap();
        drop(ordinary);
        tokio::time::timeout(std::time::Duration::from_secs(1), delivery)
            .await
            .unwrap()
            .unwrap();
        let messages = channel.messages.lock().await;
        assert_eq!(messages.len(), 2, "complete lifecycle output: {messages:?}");
        assert_eq!(
            messages[1],
            super::super::super::render_goal_response(if disable {
                &GoalResponse::Disabled
            } else {
                &GoalResponse::BudgetRequired
            })
        );
        drop(messages);
        assert!(store.current_goal_for_session(key).await.unwrap().is_none());
    }
}

async fn boundary_fixture() -> (
    MatrixGoalSessionDriver,
    GoalIngressContext,
    Arc<tokio::sync::Mutex<()>>,
) {
    let context =
        super::super::super::test_channel_ctx_with_backend(Arc::new(FailingAppendSessionBackend));
    let key = "matrix_boundary_fixture";
    let foreground = foreground_lock(&context.persist_locks, key);
    let command_lease = goal_command_lock(&context.persist_locks, key)
        .lock_owned()
        .await;
    let ingress = GoalIngressContext::trusted(
        GoalSessionKey::matrix(key).unwrap(),
        context.agent_alias.to_string(),
        "matrix:fixture:room",
        GoalIngressPrincipal::Matrix {
            raw_mxid: "@fixture:example.test".into(),
        },
    )
    .unwrap();
    let driver = MatrixGoalSessionDriver::new(
        context,
        key.into(),
        "@fixture:example.test".into(),
        "matrix:fixture:room".into(),
        ChannelMessage::new(
            "fixture-event",
            "@fixture:example.test",
            "room",
            "/goal resume",
            "matrix",
            1,
        ),
        command_lease,
    )
    .unwrap();
    (driver, ingress, foreground)
}

#[tokio::test]
async fn matrix_goal_boundary_try_retains_the_real_foreground_guard() {
    let (driver, ingress, foreground) = boundary_fixture().await;
    let ordinary_turn = foreground.clone().lock_owned().await;
    assert!(driver.try_turn_boundary(&ingress).await.unwrap().is_none());
    drop(ordinary_turn);
    let boundary = driver.try_turn_boundary(&ingress).await.unwrap().unwrap();
    assert!(foreground.try_lock().is_err());
    drop(boundary);
    assert!(foreground.try_lock().is_ok());
}

#[tokio::test]
async fn matrix_goal_boundary_waits_for_the_real_ordinary_turn() {
    let (driver, ingress, foreground) = boundary_fixture().await;
    let ordinary_turn = foreground.clone().lock_owned().await;
    let checkpoint = GoalParentTurnCheckpoint::new();
    let mut waiting = Box::pin(driver.await_turn_boundary(&ingress, &checkpoint));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiting)
            .await
            .is_err()
    );
    drop(ordinary_turn);
    let boundary = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(foreground.try_lock().is_err());
    drop(boundary);
    assert!(foreground.try_lock().is_ok());
}

#[tokio::test]
async fn matrix_goal_boundary_closed_wait_never_acquires_foreground() {
    let (driver, ingress, foreground) = boundary_fixture().await;
    let checkpoint = GoalParentTurnCheckpoint::new();
    checkpoint.close();
    let error = driver
        .await_turn_boundary(&ingress, &checkpoint)
        .await
        .unwrap_err();
    assert!(error.is::<zeroclaw_goal::GoalCheckpointClosed>());
    assert!(foreground.try_lock().is_ok());
}

#[tokio::test]
async fn matrix_goal_boundary_rejects_stale_principal_on_both_paths() {
    let (driver, ingress, foreground) = boundary_fixture().await;
    let stale = GoalIngressContext::trusted(
        ingress.session_key().clone(),
        ingress.agent(),
        ingress.route(),
        GoalIngressPrincipal::Matrix {
            raw_mxid: "@other:example.test".into(),
        },
    )
    .unwrap();
    assert!(driver.try_turn_boundary(&stale).await.is_err());
    assert!(
        driver
            .await_turn_boundary(&stale, &GoalParentTurnCheckpoint::new())
            .await
            .is_err()
    );
    assert!(foreground.try_lock().is_ok());
}

#[tokio::test]
async fn matrix_goal_queued_acknowledgement_failure_abandons_without_transition() {
    queued_control_abandonment_fixture(QueuedControlExit::UnavailableChannel).await;
}

#[tokio::test]
async fn matrix_goal_queued_send_error_abandons_without_transition() {
    queued_control_abandonment_fixture(QueuedControlExit::SendFailure).await;
}

#[tokio::test]
async fn matrix_goal_dropped_queued_control_abandons_without_transition() {
    queued_control_abandonment_fixture(QueuedControlExit::Dropped).await;
}

#[tokio::test]
async fn matrix_goal_aborted_queued_wait_abandons_without_transition() {
    queued_control_abandonment_fixture(QueuedControlExit::AbortedWait).await;
}

enum QueuedControlExit {
    UnavailableChannel,
    SendFailure,
    Dropped,
    AbortedWait,
}

async fn queued_control_abandonment_fixture(exit: QueuedControlExit) {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_goal::{GoalQueuedControl, GoalTaskRegistry};
    use zeroclaw_runtime::control_plane::SqliteTaskStore;

    let (driver, ingress, foreground) = boundary_fixture().await;
    let context = Arc::clone(&driver.context);
    let message = driver.message.clone();
    let session = ingress.session_key().durable_id();
    let ordinary_turn = foreground.clone().lock_owned().await;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::new(directory.path()).unwrap());
    let config = fixture_goal_config(&context);
    let settings =
        GoalHostSettings::from_config(&config, std::process::id(), "matrix-control-fixture-boot")
            .unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            directory.path(),
        )
        .unwrap(),
    );
    let accounting = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            store.clone(),
            tracker,
            context.agent_alias.to_string(),
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor = Arc::new(GoalExecutionSupervisor::new(Arc::new(
        GoalRuntime::new(store.clone()).execution_engine(accounting),
    )));
    let admission = supervisor
        .admit_control(
            &settings,
            ingress,
            Arc::new(driver),
            GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "fixture objective must not become a Goal".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        &admission,
        GoalControlAdmission::Queued {
            response: GoalResponse::Queued(GoalQueuedControl::Start),
            ..
        }
    ));
    assert!(
        store
            .current_goal_for_session(&session)
            .await
            .unwrap()
            .is_none()
    );
    let audit = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
    let state = || {
        audit
            .query_row(
                "SELECT state, outcome_reason FROM goal_control_intents WHERE session_id = ?1",
                [&session],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .unwrap()
    };
    assert_eq!(state(), ("pending".into(), None));
    let slot = Arc::new(tokio::sync::Mutex::new(Some(Arc::clone(&supervisor))));
    let settings_source = GoalHostSettingsSource::new(
        context.live_config.clone(),
        std::process::id(),
        "matrix-control-fixture-boot",
    )
    .unwrap();
    let command = MatrixGoalCommandSubmission {
        supervisor: Arc::clone(&supervisor),
        supervisor_slot: Arc::clone(&slot),
        settings_source,
        context: Arc::clone(&context),
        submission: admission,
        history_key: session.clone(),
    };

    let expected_reason = match exit {
        QueuedControlExit::UnavailableChannel => {
            // Unavailable native delivery must abandon promptly, not await
            // the ordinary turn or create an unacknowledged Goal.
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                deliver_matrix_goal_command(command, None, message),
            )
            .await
            .unwrap();
            "delivery_failed"
        }
        QueuedControlExit::SendFailure => {
            let channel = Arc::new(MatrixGoalErrorRecordingChannel {
                fail_sends: true,
                ..Default::default()
            });
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                deliver_matrix_goal_command(command, Some(channel.clone()), message),
            )
            .await
            .unwrap();
            assert_eq!(
                channel.messages.lock().await.as_slice(),
                [super::super::super::render_goal_response(
                    &GoalResponse::Queued(GoalQueuedControl::Start,)
                )]
            );
            "delivery_failed"
        }
        QueuedControlExit::Dropped => {
            // The tracked task may be dropped before acknowledgement. Its
            // owned continuation must settle the audit intent without ever
            // turning that audit row into lifecycle authority.
            drop(command);
            "control_dropped"
        }
        QueuedControlExit::AbortedWait => {
            let channel = Arc::new(MatrixGoalErrorRecordingChannel::default());
            let delivery_channel = channel.clone();
            let continuation = zeroclaw_spawn::spawn!(async move {
                deliver_matrix_goal_command(command, Some(delivery_channel), message).await;
            });
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while channel.messages.lock().await.is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(state(), ("pending".into(), None));
            assert!(
                !continuation.is_finished(),
                "the acknowledged control must still wait for the ordinary turn"
            );
            continuation.abort();
            assert!(continuation.await.unwrap_err().is_cancelled());
            assert_eq!(channel.messages.lock().await.len(), 1);
            "control_dropped"
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if state() == ("abandoned".into(), Some(expected_reason.into())) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        store
            .current_goal_for_session(&session)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        foreground.try_lock().is_err(),
        "the ordinary turn still owns its boundary"
    );
    assert!(
        goal_command_lock(&context.persist_locks, &session)
            .try_lock_owned()
            .is_ok()
    );
    assert!(Arc::ptr_eq(
        slot.lock().await.as_ref().unwrap(),
        &supervisor
    ));
    drop(ordinary_turn);
    assert!(foreground.try_lock().is_ok());
}
