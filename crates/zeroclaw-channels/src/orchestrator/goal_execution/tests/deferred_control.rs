use super::*;

/// Fail only the late lifecycle notice, after accepting the initial acknowledgement.
#[derive(Default)]
struct RevisionDeliveryChannel {
    messages: tokio::sync::Mutex<Vec<String>>,
    fail_late: bool,
}

impl zeroclaw_api::attribution::Attributable for RevisionDeliveryChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Matrix)
    }

    fn alias(&self) -> &str {
        "revision-delivery-fixture"
    }
}

#[async_trait]
impl Channel for RevisionDeliveryChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let mut messages = self.messages.lock().await;
        messages.push(message.content.clone());
        if self.fail_late && messages.len() == 2 {
            bail!("synthetic late lifecycle delivery failure");
        }
        Ok(())
    }

    async fn listen(&self, _: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn matrix_goal_revision_pause_retires_only_after_delivery_and_explicit_retry_runs() {
    exercise_matrix_revision_delivery(false, false, false).await;
}

#[tokio::test]
async fn matrix_goal_revision_pause_delivery_failure_keeps_state_and_explicit_retry_runs() {
    exercise_matrix_revision_delivery(true, false, false).await;
}

#[tokio::test]
async fn matrix_registered_resume_continues_after_unrelated_publication() {
    exercise_matrix_revision_delivery(false, true, false).await;
}

#[tokio::test]
async fn matrix_registered_resume_rejects_removed_binding() {
    exercise_matrix_revision_delivery(false, true, true).await;
}

async fn exercise_matrix_revision_delivery(
    fail_late: bool,
    publish_after_registration: bool,
    remove_binding: bool,
) {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_infra::session_backend::SessionBackend;
    use zeroclaw_runtime::control_plane::TaskStatus;

    let directory = tempfile::tempdir().unwrap();
    let verifier = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "{\"decision\":\"complete\",\"reason\":\"done\",\"blockers\":[]}"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
        })))
        .expect(if remove_binding { 0 } else { 1 })
        .mount(&verifier)
        .await;
    let backend = Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    );
    let channel = Arc::new(RevisionDeliveryChannel {
        fail_late,
        ..Default::default()
    });
    let provider = Arc::new(GoalPolicyProvider {
        requests: Mutex::new(Vec::new()),
        tool_rounds: 0,
        calls_per_round: 0,
    });
    let mut context = super::super::super::test_channel_ctx_with_backend_channel_and_provider(
        backend.clone(),
        channel.clone(),
        provider.clone(),
        0,
    );
    let ctx = Arc::get_mut(&mut context).unwrap();
    let mut config = fixture_goal_config(ctx);
    config.data_dir = directory.path().to_path_buf();
    config.heartbeat.enabled = false;
    config.goal.default_token_limit = Some(1000);
    config.goal.verifier.model_provider = "openai.revision-verifier".into();
    config.goal.verifier.model = Some("revision-verifier".into());
    config
        .risk_profiles
        .insert("revision-fixture".into(), Default::default());
    let worker = config
        .providers
        .models
        .ensure("custom", "revision-worker")
        .unwrap();
    worker.model = Some("revision-worker".into());
    worker.live_pricing = false;
    worker.pricing.insert("revision-worker.input".into(), 0.0);
    worker.pricing.insert("revision-worker.output".into(), 0.0);
    let verifier_config = config
        .providers
        .models
        .ensure("openai", "revision-verifier")
        .unwrap();
    verifier_config.api_key = Some("fixture-key".into());
    verifier_config.uri = Some(verifier.uri());
    verifier_config.kind = Some("openai-compatible".into());
    verifier_config.wire_api = Some(zeroclaw_config::schema::WireApi::ChatCompletions);
    verifier_config.model = Some("revision-verifier".into());
    verifier_config.live_pricing = false;
    verifier_config
        .pricing
        .insert("revision-verifier.input".into(), 0.0);
    verifier_config
        .pricing
        .insert("revision-verifier.output".into(), 0.0);
    let agent = config.agents.get_mut("test").unwrap();
    agent.model_provider = "custom.revision-worker".into();
    agent.risk_profile = "revision-fixture".into();
    config.validate().unwrap();
    let tracker = Arc::new(CostTracker::new(config.cost.clone(), directory.path()).unwrap());
    ctx.cost_tracking = Some(super::super::super::ChannelCostTrackingState {
        tracker: tracker.clone(),
        model_provider_pricing: Arc::new(build_type_level_model_provider_pricing(&config)),
        agent_alias: ctx.agent_alias.clone(),
    });
    ctx.model_provider_ref = Arc::new("custom.revision-worker".into());
    ctx.model = Arc::new("revision-worker".into());
    ctx.agent_cfg = Arc::new(config.agents["test"].clone());
    ctx.prompt_config = Arc::new(config.clone());
    let live = zeroclaw_config::live::LiveConfig::new(config.clone());
    ctx.live_config = live.handle();
    let mut message = ChannelMessage::new(
        "revision-start",
        "fixture-user",
        "room",
        "/goal start bounded repair",
        "matrix",
        1,
    );
    message.channel_alias = Some("test".into());
    let key = "matrix_revision_delivery_fixture";
    let control = install_matrix_test_control_plane(&context, directory.path(), key);
    let store = control.goal_store().unwrap();
    backend
        .append(key, &ChatMessage::user("preserved history"))
        .unwrap();
    let before_history = serde_json::to_value(backend.try_load(key).unwrap()).unwrap();
    let command = submit_matrix_goal(
        context.clone(),
        key.into(),
        message.clone(),
        GoalCommand::Start {
            budget: GoalBudgetSelection::Defaults,
            objective: "bounded repair".into(),
        },
    )
    .await
    .unwrap();
    let initial = command.response().clone();
    let GoalResponse::Started(started) = &initial else {
        panic!("managed Start must be admitted")
    };
    let task_id = started.task_id.clone();
    let epoch = started.execution_epoch;
    let slot = command.supervisor_slot.clone();
    let owner = command.supervisor.clone();
    config.goal.include_channel_turn_context = !config.goal.include_channel_turn_context;
    config.validate().unwrap();
    live.publish(live.next_revision().unwrap(), config.clone())
        .unwrap();
    deliver_matrix_goal_command(command, Some(channel.clone()), message.clone()).await;
    let paused = store.current_goal_for_session(key).await.unwrap().unwrap();
    let goal = store.get_goal_task(&task_id).await.unwrap().unwrap();
    assert_eq!(paused.id, task_id);
    assert_eq!(paused.status, TaskStatus::Paused);
    assert_eq!(paused.execution_epoch, epoch + 1);
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::ExternalDependency));
    assert_eq!(goal.objective, "bounded repair");
    assert_eq!(goal.effective_token_limit, Some(1000));
    assert_eq!(
        serde_json::to_value(backend.try_load(key).unwrap()).unwrap(),
        before_history
    );
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(verifier.received_requests().await.unwrap().is_empty());
    assert_eq!(
        tracker
            .get_summary_for_task(&task_id)
            .unwrap()
            .request_count,
        0
    );
    let messages = channel.messages.lock().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0],
        super::super::super::render_goal_response(&initial)
    );
    assert_eq!(
        messages[1],
        zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-paused")
    );
    drop(messages);
    assert_eq!(slot.lock().await.is_some(), fail_late);
    if fail_late {
        assert!(Arc::ptr_eq(slot.lock().await.as_ref().unwrap(), &owner));
    }

    let refused_response = "Refused line one.\nRefused line two.";
    let refused = submit_matrix_goal(
        context.clone(),
        key.into(),
        message.clone(),
        GoalCommand::Resume {
            response: Some(refused_response.into()),
        },
    )
    .await
    .unwrap();
    let refused_initial = refused.response().clone();
    let GoalResponse::Resumed(refused_projection) = &refused_initial else {
        panic!("Resume must be admitted before publication")
    };
    assert_eq!(refused_projection.task_id, task_id);
    let refused_slot = refused.supervisor_slot.clone();
    config.goal.include_channel_turn_context = !config.goal.include_channel_turn_context;
    live.publish(live.next_revision().unwrap(), config.clone())
        .unwrap();
    deliver_matrix_goal_command(refused, Some(channel.clone()), message.clone()).await;
    let paused = store.current_goal_for_session(key).await.unwrap().unwrap();
    assert_eq!(
        paused.execution_epoch,
        refused_projection.execution_epoch + 1
    );
    assert_eq!(paused.status, TaskStatus::Paused);
    assert!(refused_slot.lock().await.is_none());
    assert_eq!(
        serde_json::to_value(backend.try_load(key).unwrap()).unwrap(),
        before_history
    );
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(verifier.received_requests().await.unwrap().is_empty());
    let messages = channel.messages.lock().await;
    assert_eq!(messages.len(), 4);
    assert_eq!(
        messages[2],
        super::super::super::render_goal_response(&refused_initial)
    );
    assert_eq!(
        messages[3],
        zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-paused")
    );
    drop(messages);

    // A refused command contributes no response text. Only this explicit retry appends it.
    let response = "Use option B.\nKeep the repair bounded.";
    message.content = format!("/goal resume {response}");
    let resumed = submit_matrix_goal(
        context.clone(),
        key.into(),
        message.clone(),
        GoalCommand::Resume {
            response: Some(response.into()),
        },
    )
    .await
    .unwrap();
    let GoalResponse::Resumed(projection) = resumed.response() else {
        panic!("fresh explicit Resume must admit the persisted task")
    };
    assert_eq!(projection.task_id, task_id);
    assert_eq!(projection.execution_epoch, paused.execution_epoch + 1);
    let retry_scope = GoalExecutionScope::new(&task_id, key, projection.execution_epoch).unwrap();
    let retry_owner = resumed.supervisor.clone();
    let acquisition = publish_after_registration.then(|| {
        super::super::super::foreground::install_goal_test_execution_barrier(
            &context.persist_locks,
            key,
        )
    });
    deliver_matrix_goal_command(resumed, Some(channel.clone()), message).await;
    if let Some(acquisition) = acquisition {
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            acquisition.entered.notified(),
        )
        .await
        .expect("registered worker must reach session acquisition");
        let foreground =
            super::super::super::foreground::foreground_lock(&context.persist_locks, key)
                .lock_owned()
                .await;
        let captured = live.handle().revision();
        if remove_binding {
            config.channels.matrix.remove("test");
        } else {
            config.goal.include_channel_turn_context = !config.goal.include_channel_turn_context;
        }
        live.publish(live.next_revision().unwrap(), config).unwrap();
        assert!(live.handle().revision().succeeds_within_epoch(&captured));
        acquisition.release.notify_one();
        drop(foreground);
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        retry_owner.drain(&retry_scope),
    )
    .await
    .unwrap();
    if remove_binding {
        let error = result.unwrap_err();
        assert!(format!("{error:#}").contains("Goal policy changed"));
        let failed = store.current_goal_for_session(key).await.unwrap().unwrap();
        assert_eq!(failed.id, task_id);
        assert_eq!(failed.status, TaskStatus::Failed);
        assert!(provider.requests.lock().unwrap().is_empty());
        assert!(verifier.received_requests().await.unwrap().is_empty());
        let history = backend.try_load(key).unwrap();
        assert!(
            !history
                .iter()
                .any(|row| row.role == "user" && row.content == response)
        );
        let messages = channel.messages.lock().await;
        let failure = zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-failure-executor");
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.contains(&failure))
                .count(),
            1
        );
        return;
    }
    let outcome = result.unwrap();
    assert!(matches!(
        outcome,
        zeroclaw_goal::GoalExecutionOutcome::Completed
    ));
    let completed = store.current_goal_for_session(key).await.unwrap().unwrap();
    assert_eq!(completed.id, task_id);
    assert_eq!(completed.status, TaskStatus::Completed);
    assert_eq!(completed.execution_epoch, retry_scope.execution_epoch() + 1);
    if publish_after_registration {
        let messages = channel.messages.lock().await;
        let paused = zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-paused");
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.as_str() == paused.as_str())
                .count(),
            2
        );
        let failure = zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-failure-executor");
        assert!(!messages.iter().any(|message| message.contains(&failure)));
    }
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(
            requests[0]
                .iter()
                .filter(|row| row.role == "user" && row.content == response)
                .count(),
            1
        );
        assert!(
            requests[0]
                .iter()
                .any(|row| row.content == "preserved history")
        );
        assert!(
            !requests[0]
                .iter()
                .any(|row| row.content == refused_response)
        );
    }
    assert_eq!(verifier.received_requests().await.unwrap().len(), 1);
    let history = backend.try_load(key).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|row| row.role == "user" && row.content == response)
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|row| row.role == "assistant" && row.content == "policy candidate")
            .count(),
        1
    );
    assert!(history.iter().any(|row| row.content == "preserved history"));
    assert!(!history.iter().any(|row| row.content == refused_response));
    assert_eq!(
        tracker
            .get_summary_for_task(&task_id)
            .unwrap()
            .request_count,
        2
    );
    let reopened = CostTracker::new(Default::default(), directory.path()).unwrap();
    assert_eq!(
        reopened
            .get_summary_for_task(&task_id)
            .unwrap()
            .request_count,
        2
    );
    let ledger = std::fs::read_to_string(directory.path().join("state/costs.jsonl")).unwrap();
    let records: Vec<zeroclaw_config::cost::CostRecord> = ledger
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records.len(),
        2,
        "ordinary tracking must not duplicate Goal accounting"
    );
    assert!(
        records
            .iter()
            .all(|record| record.task_id.as_deref() == Some(task_id.as_str()))
    );
    verifier.verify().await;
}

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
    let mut message = ChannelMessage::new(
        "event",
        "fixture-user",
        "room",
        "/goal start fixture",
        "matrix",
        1,
    );
    message.channel_alias = Some("test".into());
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
    let execution_epoch = projection.execution_epoch;
    config.goal.enabled = false;
    live.publish(live.next_revision().unwrap(), config).unwrap();
    // Raw publication outside the policy cutover is a stale-capture control:
    // the post-response launch refuses before any model call and settles the
    // admitted epoch through the classified launch-failure owner (Goal-owner revocation contract).
    let error = tokio::time::timeout(std::time::Duration::from_secs(1), command.after_response())
        .await
        .unwrap()
        .expect_err("a policy publication between response and launch must refuse execution");
    assert!(error.is::<zeroclaw_goal::GoalLaunchFailureReported>());
    assert!(
        format!("{error:#}").contains("Goal policy changed while this command was being admitted")
    );
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
    assert_eq!(
        task.status,
        zeroclaw_runtime::control_plane::TaskStatus::Failed
    );
    assert_eq!(task.execution_epoch, execution_epoch + 1);
    assert_eq!(
        store
            .terminal_reason_for_session_goal(scope.task_id(), key)
            .await
            .unwrap()
            .as_deref(),
        Some("executor_start_failed")
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
        let mut message = ChannelMessage::new(
            "event",
            "fixture-user",
            "room",
            "/goal start fixture",
            "matrix",
            1,
        );
        message.channel_alias = Some("test".into());
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
        let mut message = ChannelMessage::new(
            "event",
            "fixture-user",
            "room",
            "/goal start fixture",
            "matrix",
            1,
        );
        message.channel_alias = Some("test".into());
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
        submission: admission,
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
