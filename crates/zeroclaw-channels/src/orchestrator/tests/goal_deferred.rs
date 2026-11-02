use super::*;

#[tokio::test]
async fn matrix_goal_stop_wrapper_preserves_non_matrix_no_op() {
    let context = matrix_control_test_ctx();
    let mut message = matrix_msg("@fixture:example.test");
    message.channel = "test-channel".into();
    assert!(
        goal_execution::submit_matrix_goal_stop(context, message)
            .await
            .unwrap()
            .is_none()
    );
}

#[derive(Default)]
struct StopCancellationProbeProvider {
    probe: GatedProbeModelProvider,
    dropped: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl ModelProvider for StopCancellationProbeProvider {
    async fn chat_with_system(
        &self,
        system: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        struct DropProbe<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for DropProbe<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _drop_probe = (!message.starts_with("Decide whether the assistant should send"))
            .then(|| DropProbe(&self.dropped));
        self.probe
            .chat_with_system(system, message, model, temperature)
            .await
    }
}

impl zeroclaw_api::attribution::Attributable for StopCancellationProbeProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        self.probe.role()
    }

    fn alias(&self) -> &str {
        "stop-cancellation-probe"
    }
}

/// Ordinary cancellation precedes the Goal lock, even when the durable
/// session owns a Running Goal without a resident parent worker.
#[tokio::test]
async fn matrix_goal_stop_cancels_ordinary_turn_before_goal_lock() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stop_cancels_ordinary_turn_before_goal_lock
    ))
    .await
    {
        return;
    }
    use zeroclaw_goal::{GoalTaskRecord, GoalTransitionResult};
    use zeroclaw_runtime::control_plane::{ControlPlaneHandle, TaskRecord, TaskStatus};

    let directory = tempfile::tempdir().unwrap();
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(StopCancellationProbeProvider::default());
    provider.probe.hold("m1");
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider = "custom.fixture".into();
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    let mut message = matrix_msg("@fixture:example.test");
    message.id = "m1".into();
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle);
    let task: TaskRecord = serde_json::from_value(serde_json::json!({
        "id": "fixture-running-stop", "kind": "goal", "status": "running",
        "agent": context.agent_alias.as_str(), "session_id": key,
        "principal_id": message.sender, "execution_epoch": 1,
        "originator_route": format!("matrix:{}:{}", message.channel_alias.as_deref().unwrap_or_default(), message.reply_target),
        "started_at": chrono::Utc::now().to_rfc3339(),
    }))
    .unwrap();
    assert_eq!(
        registry
            .create_or_replace_session_goal(
                task,
                GoalTaskRecord {
                    task_id: "fixture-running-stop".into(),
                    objective: "bounded cancellation fixture".into(),
                    ..Default::default()
                }
            )
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    let command_lease = foreground::goal_command_lock(&context.persist_locks, &key)
        .lock_owned()
        .await;
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let router = AgentRouter::single(context);
    let dispatch = zeroclaw_spawn::spawn!(run_message_dispatch_loop(receiver, router, 4));
    sender.send(message.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while provider.probe.entered_ids().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    message.id = "fixture-stop".into();
    message.content = "/stop".into();
    sender.send(message).await.unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(2), async {
        while !provider.dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    assert_eq!(
        registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    drop(command_lease);
    provider.probe.release("m1");
    drop(sender);
    tokio::time::timeout(Duration::from_secs(5), dispatch)
        .await
        .unwrap()
        .unwrap();
    assert!(
        cancelled,
        "ordinary provider future survived the inline stop boundary"
    );
    assert_eq!(
        registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Paused
    );
    assert!(channel.sent_messages.lock().await.iter().any(|reply| {
        reply
            .content
            .contains(&channel_runtime_cli_string("goal-mode-paused"))
    }));
}

/// A held Goal command lease must not stall another Matrix conversation's
/// ordinary lane or its immediate cancellation acknowledgement.
#[tokio::test]
async fn matrix_goal_stop_lock_wait_preserves_other_conversation_progress() {
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GatedProbeModelProvider::default());
    provider.hold("m1");
    let context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        matrix_control_test_config(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    let mut first = matrix_msg("@fixture:example.test");
    first.reply_target = "!first:example.test".into();
    first.content = "/stop".into();
    let key = runtime_conversation_history_key(&context, &first);
    let command_lease = foreground::goal_command_lock(&context.persist_locks, &key)
        .lock_owned()
        .await;
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let cancellation = CancellationToken::new();
    let dispatch_cancel = cancellation.clone();
    let router = AgentRouter::single(context);
    let mut dispatch = zeroclaw_spawn::spawn!(run_message_dispatch_loop_until_cancelled(
        receiver,
        router,
        4,
        dispatch_cancel,
    ));
    sender.send(first).await.unwrap();
    let mut other = matrix_msg("@fixture:example.test");
    other.id = "m1".into();
    other.reply_target = "!second:example.test".into();
    sender.send(other.clone()).await.unwrap();
    let progressed = tokio::time::timeout(Duration::from_secs(2), async {
        while !provider.entered_ids().iter().any(|id| id == "m1") {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let acknowledged = if progressed.is_ok() {
        other.id = "fixture-second-stop".into();
        other.content = "/stop".into();
        sender.send(other.clone()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !channel.sent_messages.lock().await.iter().any(|reply| {
                reply.recipient == other.reply_target
                    && reply.content == channel_runtime_cli_string("channel-runtime-stop-sent")
            }) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok()
    } else {
        false
    };
    // Release the baseline's blocked dispatcher before asserting, so a red
    // result cannot leave fixture work alive beyond its owning runtime.
    drop(command_lease);
    provider.release("m1");
    cancellation.cancel();
    drop(sender);
    tokio::time::timeout(
        CHANNEL_GENERATION_SHUTDOWN_GRACE + Duration::from_secs(2),
        &mut dispatch,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        progressed.is_ok(),
        "another Matrix lane stalled behind /stop's Goal lock"
    );
    assert!(
        acknowledged,
        "another conversation's /stop acknowledgement stalled"
    );
}

/// Generation cancellation must retire a pending stop's lock waiter while
/// its conversation's command lease is still held by a different owner.
#[tokio::test]
async fn matrix_goal_stop_lock_wait_observes_generation_shutdown() {
    let context = matrix_control_test_ctx();
    let mut message = matrix_msg("@fixture:example.test");
    message.content = "/stop".into();
    let key = runtime_conversation_history_key(&context, &message);
    let command_lock = foreground::goal_command_lock(&context.persist_locks, &key);
    let command_lease = command_lock.clone().lock_owned().await;
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let cancellation = CancellationToken::new();
    let dispatch_cancel = cancellation.clone();
    let router = AgentRouter::single(context);
    let mut dispatch = zeroclaw_spawn::spawn!(run_message_dispatch_loop_until_cancelled(
        receiver,
        router,
        4,
        dispatch_cancel,
    ));
    sender.send(message).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut dispatch)
            .await
            .is_err()
    );
    cancellation.cancel();
    let stopped = tokio::time::timeout(
        CHANNEL_GENERATION_SHUTDOWN_GRACE + Duration::from_secs(1),
        &mut dispatch,
    )
    .await;
    let stopped_while_held = match stopped {
        Ok(result) => {
            result.unwrap();
            true
        }
        Err(_) => false,
    };
    drop(command_lease);
    drop(sender);
    if !stopped_while_held {
        tokio::time::timeout(Duration::from_secs(2), &mut dispatch)
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        command_lock.try_lock_owned().is_ok(),
        "a stop waiter survived dispatcher shutdown"
    );
    assert!(
        stopped_while_held,
        "generation shutdown waited for an unrelated Goal command lease"
    );
}

/// Non-Matrix cancellation keeps its ordinary path, even when a Goal lock
/// with the same canonical key is held; shutdown owns no Goal stop waiter.
#[tokio::test]
async fn matrix_goal_stop_non_matrix_control_avoids_goal_lock() {
    let channel = Arc::new(SendMessageRecordingChannel::default());
    let context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        Arc::new(GatedProbeModelProvider::default()),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    let message = zeroclaw_api::channel::ChannelMessage {
        channel: "test-channel".into(),
        reply_target: "fixture-peer".into(),
        content: "/stop".into(),
        ..Default::default()
    };
    let key = runtime_conversation_history_key(&context, &message);
    let command_lease = foreground::goal_command_lock(&context.persist_locks, &key)
        .lock_owned()
        .await;
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let cancellation = CancellationToken::new();
    let dispatch_cancel = cancellation.clone();
    let router = AgentRouter::single(context);
    let mut dispatch = zeroclaw_spawn::spawn!(run_message_dispatch_loop_until_cancelled(
        receiver,
        router,
        4,
        dispatch_cancel,
    ));
    sender.send(message).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !channel.sent_messages.lock().await.iter().any(|reply| {
            reply.content == channel_runtime_cli_string("channel-runtime-stop-no-task")
        }) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ordinary non-Matrix stop must not acquire a Goal lock");
    cancellation.cancel();
    drop(sender);
    tokio::time::timeout(Duration::from_millis(250), &mut dispatch)
        .await
        .expect("non-Matrix stop must leave no Goal waiter for shutdown")
        .unwrap();
    drop(command_lease);
}

/// Fail the predecessor through the ordinary model boundary, then hold a
/// successor so the command's launch and cleanup can be observed separately.
#[derive(Clone, Copy, Debug, Default)]
enum GoalFailedFirstResponse {
    #[default]
    BareError,
    CompleteEmpty,
    MissingResponseUsage,
    CompleteCandidate,
}

#[derive(Default)]
struct GoalFailedPredecessorProvider {
    calls: std::sync::atomic::AtomicUsize,
    successor_release: tokio::sync::Notify,
    hold_first: std::sync::atomic::AtomicBool,
    first_entered: tokio::sync::Notify,
    first_release: tokio::sync::Notify,
    first_response: GoalFailedFirstResponse,
}

impl GoalFailedPredecessorProvider {
    fn new(first_response: GoalFailedFirstResponse) -> Self {
        Self {
            first_response,
            ..Self::default()
        }
    }
}

fn assert_failed_predecessor_ledger(
    directory: &std::path::Path,
    tracker: &zeroclaw_runtime::cost::CostTracker,
    task_id: &str,
    known_attempt: bool,
    total_attempts: usize,
) {
    let ledger = match std::fs::read_to_string(directory.join("state/costs.jsonl")) {
        Ok(ledger) => ledger,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => panic!("fixture ledger must remain readable: {error}"),
    };
    let records: Vec<zeroclaw_config::cost::CostRecord> = ledger
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records.len(),
        total_attempts,
        "task {task_id}: one canonical writer"
    );
    assert!(
        records.iter().all(|row| row.task_id.is_some()),
        "no ordinary duplicate row"
    );
    let task_rows: Vec<_> = records
        .iter()
        .filter(|row| row.task_id.as_deref() == Some(task_id))
        .collect();
    assert_eq!(
        task_rows.len(),
        usize::from(known_attempt),
        "task {task_id}"
    );
    for row in task_rows {
        assert_eq!(row.provider_ref.as_deref(), Some("test-provider"));
        assert_eq!(row.usage.model, "test-model");
        assert_eq!(row.usage.input_tokens, 10);
        assert_eq!(row.usage.output_tokens, 2);
        assert_eq!(row.usage.total_tokens, 12);
        assert_eq!(row.usage.cost_usd, 0.0);
        assert_eq!(row.usage.unpriced_tokens, 12);
        assert!(!row.usage.pricing_available);
    }
    let reopened = zeroclaw_runtime::cost::CostTracker::new(Default::default(), directory).unwrap();
    for reader in [tracker, &reopened] {
        assert_eq!(
            reader
                .get_strict_usage_totals_for_task_with_pricing(task_id)
                .unwrap(),
            if known_attempt {
                (12, 0.0, false)
            } else {
                (0, 0.0, true)
            },
            "task {task_id}: incomplete accounting is asserted separately from its known lower bound"
        );
        let summary = reader.get_summary_for_task(task_id).unwrap();
        assert_eq!(summary.request_count, usize::from(known_attempt));
        assert_eq!(summary.total_tokens, if known_attempt { 12 } else { 0 });
        assert_eq!(
            reader
                .get_summary_in_bounds(None, None)
                .unwrap()
                .request_count,
            total_attempts
        );
    }
}

#[tokio::test]
async fn matrix_goal_stop_lock_wait_bounds_pending_goal_stops() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stop_lock_wait_bounds_pending_goal_stops
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_stop_order_and_bound(true).await;
}

#[tokio::test]
async fn matrix_goal_stop_lock_wait_precedes_later_goal_cancel() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stop_lock_wait_precedes_later_goal_cancel
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_stop_order_and_bound(false).await;
}

/// Observe durable transition order independently of response scheduling.
/// A held, billable parent settles only after the first stop has fenced it.
async fn assert_matrix_goal_stop_order_and_bound(check_bound: bool) {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_goal::GoalRuntime;
    use zeroclaw_runtime::{
        control_plane::{ControlPlaneHandle, TaskStatus},
        cost::CostTracker,
        goal_accounting::{ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory},
    };

    let directory = tempfile::tempdir().unwrap();
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GoalFailedPredecessorProvider::new(
        GoalFailedFirstResponse::CompleteCandidate,
    ));
    provider.hold_first.store(true, Ordering::SeqCst);
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider = "custom.fixture".into();
    config.providers.models.ensure("custom", "fixture").unwrap();
    config.reliability.provider_retries = 0;
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    let mut message = matrix_msg("@fixture:example.test");
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle.clone());
    let accounting = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            registry.clone(),
            Arc::new(CostTracker::new(Default::default(), directory.path()).unwrap()),
            context.agent_alias.to_string(),
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor = handle
        .goal_execution_restart()
        .new_supervisor(Arc::new(
            GoalRuntime::new(registry.clone()).execution_engine(accounting),
        ))
        .await;
    *foreground::goal_supervisor_slot(&context.persist_locks, &key)
        .lock()
        .await = Some(supervisor);
    let start = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Start {
            budget: GoalBudgetSelection::Unlimited,
            objective: "bounded cancellation fixture".into(),
        },
    )
    .await
    .unwrap();
    goal_execution::deliver_matrix_goal_command(start, Some(channel.clone()), message.clone())
        .await;
    tokio::time::timeout(Duration::from_secs(3), provider.first_entered.notified())
        .await
        .unwrap();
    assert_eq!(
        registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    let audit = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
    audit
        .execute_batch(
            "CREATE TABLE fixture_stop_transitions (status TEXT NOT NULL);
         CREATE TRIGGER fixture_record_stop_transition AFTER UPDATE OF status ON tasks
         WHEN NEW.kind = 'goal' AND OLD.status != NEW.status
         BEGIN INSERT INTO fixture_stop_transitions(status) VALUES (NEW.status); END;",
        )
        .unwrap();
    let command_lease = foreground::goal_command_lock(&context.persist_locks, &key)
        .lock_owned()
        .await;
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    let router = AgentRouter::single(context);
    let mut dispatch = zeroclaw_spawn::spawn!(run_message_dispatch_loop(receiver, router, 4));
    let stop_count = if check_bound {
        MAX_CONCURRENT_STOP_REPLIES + 1
    } else {
        1
    };
    message.content = "/stop".into();
    for index in 0..stop_count {
        message.id = format!("fixture-stop-{index}");
        sender.send(message.clone()).await.unwrap();
    }
    let overflow = if check_bound {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !channel.sent_messages.lock().await.iter().any(|reply| {
                reply.content
                    == format!(
                        "{}\n{}",
                        channel_runtime_cli_string("channel-runtime-stop-no-task"),
                        channel_runtime_cli_string("goal-mode-command-failed")
                    )
            }) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok()
    } else {
        message.id = "fixture-cancel-after-stop".into();
        message.content = "/goal cancel".into();
        sender.send(message).await.unwrap();
        true
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut dispatch)
            .await
            .is_err()
    );
    drop(command_lease);
    tokio::time::timeout(Duration::from_secs(3), async {
        while audit
            .query_row(
                "SELECT COUNT(*) FROM fixture_stop_transitions WHERE status = 'paused'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first stop must fence its running Goal before parent settlement");
    provider.first_release.notify_one();
    drop(sender);
    tokio::time::timeout(Duration::from_secs(5), &mut dispatch)
        .await
        .expect("all accepted stop and cancellation tasks must drain")
        .unwrap();
    let statuses = audit
        .prepare("SELECT status FROM fixture_stop_transitions ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        statuses,
        if check_bound {
            vec!["paused"]
        } else {
            vec!["paused", "cancelled"]
        }
    );
    let task = registry
        .current_goal_for_session(&key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        task.status,
        if check_bound {
            TaskStatus::Paused
        } else {
            TaskStatus::Cancelled
        }
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "a stop must admit no verifier or successor"
    );
    let replies = channel.sent_messages.lock().await;
    let pause = replies
        .iter()
        .position(|reply| reply.content == channel_runtime_cli_string("goal-mode-pause-pending"))
        .unwrap();
    assert_eq!(
        replies
            .iter()
            .filter(|reply| reply.content == channel_runtime_cli_string("goal-mode-pause-pending"))
            .count(),
        1
    );
    if check_bound {
        assert_eq!(
            replies
                .iter()
                .filter(|reply| reply
                    .content
                    .contains(&channel_runtime_cli_string("goal-mode-command-failed")))
                .count(),
            1
        );
        assert!(
            overflow,
            "the ninth stop waited for the conversation's held Goal lock"
        );
    } else {
        let cancel = replies
            .iter()
            .position(|reply| {
                reply
                    .content
                    .starts_with(&channel_runtime_cli_string("goal-mode-cancelled"))
                    || reply.content == channel_runtime_cli_string("goal-mode-cancel-pending")
            })
            .unwrap();
        assert!(
            pause < cancel,
            "a later Goal cancel overtook the earlier stop"
        );
        assert!(!replies.iter().any(|reply| {
            reply
                .content
                .contains(&channel_runtime_cli_string("goal-mode-command-failed"))
        }));
    }
}

impl zeroclaw_api::attribution::Attributable for GoalFailedPredecessorProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }

    fn alias(&self) -> &str {
        "failed-predecessor-fixture"
    }
}

#[async_trait::async_trait]
impl ModelProvider for GoalFailedPredecessorProvider {
    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("fixture requires the structured model boundary")
    }

    async fn chat(
        &self,
        _: zeroclaw_api::model_provider::ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<zeroclaw_api::model_provider::ChatResponse> {
        match self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => {
                self.first_entered.notify_one();
                if self.hold_first.load(std::sync::atomic::Ordering::SeqCst) {
                    self.first_release.notified().await;
                }
                match self.first_response {
                    GoalFailedFirstResponse::BareError => {
                        anyhow::bail!("scripted parent failure");
                    }
                    GoalFailedFirstResponse::CompleteEmpty => {
                        return Ok(zeroclaw_api::model_provider::ChatResponse {
                            text: None,
                            tool_calls: Vec::new(),
                            usage: Some(zeroclaw_api::model_provider::TokenUsage {
                                input_tokens: Some(10),
                                output_tokens: Some(2),
                                ..Default::default()
                            }),
                            reasoning_content: None,
                        });
                    }
                    GoalFailedFirstResponse::MissingResponseUsage => {
                        return Ok(zeroclaw_api::model_provider::ChatResponse {
                            text: Some("missing usage candidate".into()),
                            tool_calls: Vec::new(),
                            usage: None,
                            reasoning_content: None,
                        });
                    }
                    GoalFailedFirstResponse::CompleteCandidate => {}
                }
            }
            1 => self.successor_release.notified().await,
            _ => anyhow::bail!("fixture must admit no extra parent call"),
        }
        Ok(zeroclaw_api::model_provider::ChatResponse {
            text: Some("successor candidate".into()),
            tool_calls: Vec::new(),
            usage: Some(zeroclaw_api::model_provider::TokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(2),
                ..Default::default()
            }),
            reasoning_content: None,
        })
    }
}

#[tokio::test]
async fn matrix_goal_pause_now_preserves_fenced_parent_error_and_failure_notice() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_pause_now_preserves_fenced_parent_error_and_failure_notice
    ))
    .await
    {
        return;
    }
    assert_matrix_fenced_parent_failure_delivery(false, GoalFailedFirstResponse::BareError, false)
        .await;
}

#[tokio::test]
async fn matrix_goal_stop_preserves_fenced_parent_error_and_failure_notice() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stop_preserves_fenced_parent_error_and_failure_notice
    ))
    .await
    {
        return;
    }
    assert_matrix_fenced_parent_failure_delivery(true, GoalFailedFirstResponse::BareError, false)
        .await;
}

#[tokio::test]
async fn matrix_goal_pause_now_preserves_unsettled_accounting_failure_notice() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_pause_now_preserves_unsettled_accounting_failure_notice
    ))
    .await
    {
        return;
    }
    assert_matrix_fenced_parent_failure_delivery(
        false,
        GoalFailedFirstResponse::CompleteCandidate,
        true,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_pause_now_preserves_complete_empty_operator_pause() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_pause_now_preserves_complete_empty_operator_pause
    ))
    .await
    {
        return;
    }
    assert_matrix_fenced_parent_failure_delivery(
        false,
        GoalFailedFirstResponse::CompleteEmpty,
        false,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_stop_preserves_complete_empty_operator_pause() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stop_preserves_complete_empty_operator_pause
    ))
    .await
    {
        return;
    }
    assert_matrix_fenced_parent_failure_delivery(
        true,
        GoalFailedFirstResponse::CompleteEmpty,
        false,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_pause_now_preserves_missing_response_usage_failure() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_pause_now_preserves_missing_response_usage_failure
    ))
    .await
    {
        return;
    }
    assert_matrix_fenced_parent_failure_delivery(
        false,
        GoalFailedFirstResponse::MissingResponseUsage,
        false,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_stop_preserves_missing_response_usage_failure() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stop_preserves_missing_response_usage_failure
    ))
    .await
    {
        return;
    }
    assert_matrix_fenced_parent_failure_delivery(
        true,
        GoalFailedFirstResponse::MissingResponseUsage,
        false,
    )
    .await;
}

async fn assert_matrix_fenced_parent_failure_delivery(
    stop: bool,
    first_response: GoalFailedFirstResponse,
    unsettled: bool,
) {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_goal::{GoalResponse, GoalRuntime};
    use zeroclaw_runtime::{
        control_plane::{ControlPlaneHandle, TaskStatus},
        cost::CostTracker,
        goal_accounting::{ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory},
    };

    let directory = tempfile::tempdir().unwrap();
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GoalFailedPredecessorProvider::new(first_response));
    provider
        .hold_first
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider = "custom.fixture".into();
    config.providers.models.ensure("custom", "fixture").unwrap();
    config.reliability.provider_retries = 0;
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    let mut message = matrix_msg("@fixture:example.test");
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle.clone());
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            directory.path(),
        )
        .unwrap(),
    );
    let accounting = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            registry.clone(),
            tracker.clone(),
            context.agent_alias.to_string(),
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor = handle
        .goal_execution_restart()
        .new_supervisor(Arc::new(
            GoalRuntime::new(registry.clone()).execution_engine(accounting),
        ))
        .await;
    *foreground::goal_supervisor_slot(&context.persist_locks, &key)
        .lock()
        .await = Some(supervisor.clone());
    let started = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Start {
            budget: GoalBudgetSelection::Unlimited,
            objective: "bounded predecessor repair".into(),
        },
    )
    .await
    .unwrap();
    let GoalResponse::Started(started_projection) = started.response() else {
        panic!("fixture must start directly");
    };
    let predecessor_id = started_projection.task_id.clone();
    let predecessor_epoch = started_projection.execution_epoch;
    goal_execution::deliver_matrix_goal_command(started, Some(channel.clone()), message.clone())
        .await;
    tokio::time::timeout(Duration::from_secs(3), provider.first_entered.notified())
        .await
        .expect("the actual Matrix parent provider call must be held");
    let admitted = registry
        .get_goal_task(&predecessor_id)
        .await
        .unwrap()
        .unwrap();
    let pending_call_id = admitted
        .pending_call_id
        .clone()
        .expect("held physical attempt must own its pending slot");
    assert_eq!(admitted.pending_call_epoch, Some(predecessor_epoch));
    assert_eq!(
        admitted.accounting_state,
        zeroclaw_goal::GoalAccountingState::Complete
    );
    assert_failed_predecessor_ledger(directory.path(), &tracker, &predecessor_id, false, 0);
    if unsettled {
        // Refuse only the ordinary operation's settlement. The drainer can
        // still clear the exact pending slot as outcome-unknown, preserving
        // already-recorded usage rather than inventing a successful operation.
        rusqlite::Connection::open(directory.path().join("control_plane.db"))
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fixture_refuse_pending_settlement
                 BEFORE UPDATE OF pending_call_id ON goal_tasks
                 WHEN OLD.pending_call_id IS NOT NULL
                   AND NEW.pending_call_id IS NULL
                   AND NEW.accounting_state != 'outcome_unknown'
                 BEGIN SELECT RAISE(ABORT, 'fixture pending settlement unavailable'); END;",
            )
            .unwrap();
    }
    message.content = if stop { "/stop" } else { "/goal pause now" }.into();
    let paused = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Pause { immediate: true },
    )
    .await
    .unwrap();
    assert!(matches!(
        paused.response(),
        GoalResponse::TransitionPending(_)
    ));
    let delivery_channel = channel.clone();
    let mut delivery = zeroclaw_spawn::spawn!(async move {
        if stop {
            goal_execution::deliver_matrix_goal_stop(paused, Some(delivery_channel), message).await;
        } else {
            goal_execution::deliver_matrix_goal_command(paused, Some(delivery_channel), message)
                .await;
        }
    });
    let pending = channel_runtime_cli_string("goal-mode-pause-pending");
    let outcome = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !channel
                .sent_messages
                .lock()
                .await
                .iter()
                .any(|row| row.content == pending)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pause acknowledgement must precede provider settlement");
        assert_eq!(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Paused
        );
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let held = registry
            .get_goal_task(&predecessor_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            held.pending_call_id.as_deref(),
            Some(pending_call_id.as_str())
        );
        assert_eq!(held.pending_call_epoch, Some(predecessor_epoch));
    };
    let result = std::panic::AssertUnwindSafe(outcome).catch_unwind().await;
    provider.first_release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), &mut delivery)
        .await
        .expect("classified failure delivery must settle after provider release")
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    let settled = registry
        .current_goal_for_session(&key)
        .await
        .unwrap()
        .unwrap();
    let goal = registry.get_goal_task(&settled.id).await.unwrap().unwrap();
    let reason = registry
        .terminal_reason_for_session_goal(&settled.id, &key)
        .await
        .unwrap();
    let replies = channel.sent_messages.lock().await;
    let diagnostic = format!(
        "mode={first_response:?}, task={}, epoch={}, status={:?}, accounting={:?}, pending={:?}/{:?}, reason={reason:?}, sent={:?}",
        settled.id,
        settled.execution_epoch,
        settled.status,
        goal.accounting_state,
        goal.pending_call_id,
        goal.pending_call_epoch,
        replies
            .iter()
            .map(|row| row.content.as_str())
            .collect::<Vec<_>>()
    );
    let complete_empty = matches!(first_response, GoalFailedFirstResponse::CompleteEmpty);
    let missing = matches!(
        first_response,
        GoalFailedFirstResponse::MissingResponseUsage
    );
    let known_attempt = complete_empty || unsettled;
    assert_eq!(settled.id, predecessor_id, "{diagnostic}");
    assert_eq!(
        settled.execution_epoch,
        predecessor_epoch + if complete_empty { 1 } else { 2 },
        "{diagnostic}"
    );
    assert_eq!(
        settled.status,
        if complete_empty {
            TaskStatus::Paused
        } else {
            TaskStatus::Failed
        },
        "{diagnostic}"
    );
    assert_eq!(
        goal.accounting_state,
        if complete_empty {
            zeroclaw_goal::GoalAccountingState::Complete
        } else if missing {
            zeroclaw_goal::GoalAccountingState::Missing
        } else {
            zeroclaw_goal::GoalAccountingState::OutcomeUnknown
        },
        "{diagnostic}"
    );
    assert!(
        goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none(),
        "{diagnostic}"
    );
    if complete_empty {
        assert_eq!(
            goal.pause_reason,
            Some(zeroclaw_goal::GoalPauseReason::OperatorPaused),
            "{diagnostic}"
        );
        assert!(
            reason.is_none(),
            "winning pause has no terminal failure reason; {diagnostic}"
        );
    } else {
        assert_eq!(
            reason.as_deref(),
            Some(if missing {
                "accounting_missing_or_invalid"
            } else {
                "accounting_outcome_unknown"
            }),
            "{diagnostic}"
        );
    }
    assert_failed_predecessor_ledger(
        directory.path(),
        &tracker,
        &predecessor_id,
        known_attempt,
        usize::from(known_attempt),
    );
    assert!(
        context
            .session_store
            .as_ref()
            .unwrap()
            .load(&key)
            .iter()
            .all(|row| row.role != "assistant"),
        "no failed parent candidate may append; {diagnostic}"
    );
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let failure = channel_runtime_cli_string(if missing {
        "goal-mode-failure-accounting"
    } else {
        "goal-mode-failure-outcome-unknown"
    });
    let semantic_cause = "provider completed without final text or tool calls";
    let original_cause = if complete_empty {
        semantic_cause
    } else if unsettled {
        "fixture pending settlement unavailable"
    } else if missing {
        "Goal accounting is missing after a durable usage settlement"
    } else {
        "scripted parent failure"
    };
    let unsettled_diagnostic =
        "fixture pending settlement unavailable: Error code 1811: constraint failed";
    let unsettled_hint = channel_user_error_message(
        &anyhow::Error::msg(unsettled_diagnostic),
        unsettled_diagnostic,
    );
    let unsettled_original = format!("{unsettled_hint}\n{unsettled_diagnostic}");
    let original_rows: Vec<_> = replies
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            if unsettled {
                row.content == unsettled_original
            } else {
                row.content.starts_with("⚠️ Error: ")
                    && row.content.contains(original_cause)
                    && !row
                        .content
                        .contains("Goal failure lost its execution fence")
            }
        })
        .collect();
    assert_eq!(
        original_rows.len(),
        1,
        "ordinary native error exactly once; {diagnostic}"
    );
    let (original, original_row) = original_rows[0];
    assert_eq!(
        original_row.content.matches(original_cause).count(),
        1,
        "{diagnostic}"
    );
    if unsettled {
        assert_eq!(
            unsettled_hint,
            "I'm temporarily unable to reach my AI backend — please try again in a moment.",
            "the ordinary unavailable hint retains its native role; {diagnostic}"
        );
        assert_eq!(original_row.content, unsettled_original, "{diagnostic}");
    }
    if complete_empty {
        let native = zeroclaw_runtime::agent::terminal_completion_error_message(
            &anyhow::Error::new(zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion),
            None,
        )
        .unwrap();
        assert!(
            original_row
                .content
                .starts_with(&format!("⚠️ Error: {native}")),
            "{diagnostic}"
        );
    } else if !unsettled {
        let state = if missing {
            "missing"
        } else {
            "outcome-unknown"
        };
        let issue = if missing {
            "missing usage"
        } else {
            "usage outcome unknown"
        };
        let settlement = format!(
            "Goal accounting is {state} after a durable usage settlement; no further Goal model call will be admitted: accounting issue for actual provider `test-provider`, model `test-model`: {issue}"
        );
        assert!(original_row.content.contains(&settlement), "{diagnostic}");
        assert!(
            !original_row
                .content
                .contains("fixture pending settlement unavailable"),
            "{diagnostic}"
        );
        if missing {
            assert!(
                !original_row.content.contains("scripted parent failure"),
                "{diagnostic}"
            );
        }
    }
    let late = channel_runtime_cli_string(if complete_empty {
        "goal-mode-paused"
    } else {
        "goal-mode-terminal"
    });
    let ack = replies
        .iter()
        .position(|row| row.content == pending)
        .unwrap();
    let terminal = replies
        .iter()
        .position(|row| row.content.starts_with(&late))
        .unwrap_or_else(|| {
            panic!("late lifecycle response must follow ordinary error; {diagnostic}")
        });
    assert!(ack < original && original < terminal, "{diagnostic}");
    assert_eq!(
        replies.iter().filter(|row| row.content == pending).count(),
        1,
        "{diagnostic}"
    );
    assert_eq!(
        replies
            .iter()
            .filter(|row| row.content.starts_with(&late))
            .count(),
        1,
        "{diagnostic}"
    );
    if complete_empty {
        assert!(
            !replies.iter().any(|row| row
                .content
                .contains("Goal failure lost its execution fence")),
            "{diagnostic}"
        );
        for key in [
            "goal-mode-failure-accounting",
            "goal-mode-failure-pricing",
            "goal-mode-failure-outcome-unknown",
            "goal-mode-failure-executor",
            "goal-mode-failure-executor-start",
            "goal-mode-failure-agent-operation",
            "goal-mode-failure-tool-results-unpaired",
            "goal-mode-failure-empty-agent-response",
            "goal-mode-failure-verifier-operation",
            "goal-mode-failure-verifier-response",
            "goal-mode-failure-candidate-persistence",
            "goal-mode-failure-other",
        ] {
            assert!(
                !replies
                    .iter()
                    .any(|row| row.content == channel_runtime_cli_string(key)),
                "no Failed notice for the winning pause; {diagnostic}"
            );
        }
    } else {
        let notices: Vec<_> = replies
            .iter()
            .enumerate()
            .filter(|(_, row)| row.content == failure)
            .collect();
        assert_eq!(notices.len(), 1, "{diagnostic}");
        let parked: Vec<_> = replies
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.content
                    .contains("Goal failure lost its execution fence")
            })
            .collect();
        assert_eq!(parked.len(), 1, "{diagnostic}");
        assert!(
            parked[0].1.content.starts_with("⚠️ Error: "),
            "{diagnostic}"
        );
        if unsettled {
            assert_eq!(
                parked[0].1.content,
                format!(
                    "⚠️ Error: Goal failure lost its execution fence: Goal parent operation failed: {unsettled_diagnostic}"
                ),
                "the handed aggregate has its own exact warning role; {diagnostic}"
            );
            assert_eq!(
                parked[0].1.content.matches(unsettled_diagnostic).count(),
                1,
                "aggregate retains the complete original diagnostic once, not its hint; {diagnostic}"
            );
        } else {
            assert!(
                parked[0]
                    .1
                    .content
                    .contains(&original_row.content["⚠️ Error: ".len()..]),
                "aggregate must retain the complete original diagnostic; {diagnostic}"
            );
        }
        assert_eq!(
            parked[0]
                .1
                .content
                .matches("Goal failure lost its execution fence")
                .count(),
            1,
            "{diagnostic}"
        );
        assert!(
            original < parked[0].0 && parked[0].0 < notices[0].0 && notices[0].0 < terminal,
            "{diagnostic}"
        );
    }
    assert_eq!(
        replies
            .iter()
            .filter(|row| row.content.starts_with("⚠️ Error: "))
            .count(),
        if complete_empty || unsettled { 1 } else { 2 },
        "warning roles are distinct from the ordinary backend hint; {diagnostic}"
    );
    if unsettled {
        assert_eq!(
            replies
                .iter()
                .filter(|row| row.content.starts_with(&unsettled_hint))
                .count(),
            1,
            "exactly one ordinary backend-hint row; {diagnostic}"
        );
    }
    assert_eq!(
        replies.len(),
        if complete_empty { 4 } else { 6 },
        "fixed lifecycle and native-error reply count; {diagnostic}"
    );
    assert!(
        replies
            .iter()
            .all(|row| row.content != channel_runtime_cli_string("goal-mode-command-failed"))
    );
    drop(replies);
    supervisor.dispose_session(&key).await.unwrap();
}

#[tokio::test]
async fn matrix_goal_failure_delivery_waits_for_ordinary_lane_without_blocking_other_conversation()
{
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_failure_delivery_waits_for_ordinary_lane_without_blocking_other_conversation
    ))
    .await
    {
        return;
    }
    assert_matrix_failure_delivery_behind_ordinary_lane(MatrixFailureDeliveryCase::Drainer).await;
}

#[tokio::test]
async fn matrix_goal_start_queues_behind_failure_delivery_and_ordinary_lane() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_start_queues_behind_failure_delivery_and_ordinary_lane
    ))
    .await
    {
        return;
    }
    assert_matrix_failure_delivery_behind_ordinary_lane(MatrixFailureDeliveryCase::DrainerStart)
        .await;
}

#[tokio::test]
async fn matrix_goal_new_withdraws_failure_delivery_before_ordinary_lane_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_withdraws_failure_delivery_before_ordinary_lane_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_failure_delivery_behind_ordinary_lane(MatrixFailureDeliveryCase::TailReset).await;
}

#[tokio::test]
async fn matrix_goal_start_waits_for_worker_tail_original_and_failure_notice() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_start_waits_for_worker_tail_original_and_failure_notice
    ))
    .await
    {
        return;
    }
    assert_matrix_failure_delivery_behind_ordinary_lane(MatrixFailureDeliveryCase::TailStart).await;
}

#[derive(Clone, Copy, Debug)]
enum MatrixFailureDeliveryCase {
    Drainer,
    DrainerStart,
    TailReset,
    TailStart,
    AckFailure,
}

#[tokio::test]
async fn matrix_goal_ack_failure_delivery_releases_goal_command_lock() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_ack_failure_delivery_releases_goal_command_lock
    ))
    .await
    {
        return;
    }
    assert_matrix_failure_delivery_behind_ordinary_lane(MatrixFailureDeliveryCase::AckFailure)
        .await;
}

#[derive(Default)]
struct GoalOrdinaryLaneFailureProvider {
    goal: GoalFailedPredecessorProvider,
    ordinary_entered: tokio::sync::Notify,
    ordinary_release: tokio::sync::Notify,
    unrelated_entered: tokio::sync::Notify,
}

impl zeroclaw_api::attribution::Attributable for GoalOrdinaryLaneFailureProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        self.goal.role()
    }

    fn alias(&self) -> &str {
        "ordinary-lane-failure-fixture"
    }
}

#[async_trait::async_trait]
impl ModelProvider for GoalOrdinaryLaneFailureProvider {
    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn chat_with_system(
        &self,
        _: Option<&str>,
        message: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        if message.starts_with("Decide whether the assistant should send") {
            return Ok("REPLY".into());
        }
        anyhow::bail!("fixture requires the structured model boundary")
    }

    async fn chat(
        &self,
        request: zeroclaw_api::model_provider::ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<zeroclaw_api::model_provider::ChatResponse> {
        let user = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user");
        let text = match user.map(|message| message.content.as_str()) {
            Some(user) if user.ends_with("fixture-held-ordinary-lane") => {
                self.ordinary_entered.notify_one();
                self.ordinary_release.notified().await;
                "ordinary lane finished"
            }
            Some(user) if user.ends_with("fixture-unrelated-conversation") => {
                self.unrelated_entered.notify_one();
                "unrelated conversation finished"
            }
            _ => return self.goal.chat(request, model, temperature).await,
        };
        Ok(zeroclaw_api::model_provider::ChatResponse {
            text: Some(text.into()),
            tool_calls: Vec::new(),
            usage: Some(zeroclaw_api::model_provider::TokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(2),
                ..Default::default()
            }),
            reasoning_content: None,
        })
    }
}

async fn assert_matrix_failure_delivery_behind_ordinary_lane(case: MatrixFailureDeliveryCase) {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_goal::{GoalQueuedControl, GoalResponse, GoalRuntime};
    use zeroclaw_runtime::{
        control_plane::{ControlPlaneHandle, TaskStatus},
        cost::CostTracker,
        goal_accounting::{ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory},
    };

    let start_successor = matches!(
        case,
        MatrixFailureDeliveryCase::DrainerStart | MatrixFailureDeliveryCase::TailStart
    );
    let reset_session = matches!(case, MatrixFailureDeliveryCase::TailReset);
    let ack_failure = matches!(case, MatrixFailureDeliveryCase::AckFailure);
    let worker_tail = matches!(
        case,
        MatrixFailureDeliveryCase::TailReset | MatrixFailureDeliveryCase::TailStart
    );

    let directory = tempfile::tempdir().unwrap();
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GoalOrdinaryLaneFailureProvider {
        goal: GoalFailedPredecessorProvider::new(if worker_tail {
            GoalFailedFirstResponse::CompleteEmpty
        } else {
            GoalFailedFirstResponse::BareError
        }),
        ..Default::default()
    });
    provider
        .goal
        .hold_first
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider = "custom.fixture".into();
    config.providers.models.ensure("custom", "fixture").unwrap();
    config.reliability.provider_retries = 0;
    config
        .channels
        .matrix
        .get_mut("default")
        .unwrap()
        .interrupt_on_new_message = false;
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    let mut message = matrix_msg("@fixture:example.test");
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle.clone());
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            directory.path(),
        )
        .unwrap(),
    );
    let accounting = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            registry.clone(),
            tracker.clone(),
            context.agent_alias.to_string(),
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor = handle
        .goal_execution_restart()
        .new_supervisor(Arc::new(
            GoalRuntime::new(registry.clone()).execution_engine(accounting),
        ))
        .await;
    *foreground::goal_supervisor_slot(&context.persist_locks, &key)
        .lock()
        .await = Some(supervisor.clone());
    let started = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Start {
            budget: GoalBudgetSelection::Unlimited,
            objective: "bounded predecessor repair".into(),
        },
    )
    .await
    .unwrap();
    let GoalResponse::Started(predecessor) = started.response() else {
        panic!("fixture must start directly");
    };
    let predecessor_id = predecessor.task_id.clone();
    let predecessor_epoch = predecessor.execution_epoch;
    goal_execution::deliver_matrix_goal_command(started, Some(channel.clone()), message.clone())
        .await;
    tokio::time::timeout(
        Duration::from_secs(3),
        provider.goal.first_entered.notified(),
    )
    .await
    .unwrap();
    let admitted = registry
        .get_goal_task(&predecessor_id)
        .await
        .unwrap()
        .unwrap();
    assert!(admitted.pending_call_id.is_some());
    assert_eq!(admitted.pending_call_epoch, Some(predecessor_epoch));
    assert_eq!(
        admitted.accounting_state,
        zeroclaw_goal::GoalAccountingState::Complete
    );
    assert_failed_predecessor_ledger(directory.path(), &tracker, &predecessor_id, false, 0);
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let router = AgentRouter::single(context.clone());
    let mut dispatch = zeroclaw_spawn::spawn!(run_message_dispatch_loop(receiver, router, 4));
    let mut ordinary = message.clone();
    ordinary.id = "fixture-ordinary".into();
    ordinary.content = "fixture-held-ordinary-lane".into();
    sender.send(ordinary).await.unwrap();
    // Give the admitted lane its foreground wait while the Goal parent holds
    // that lease. The provider signal below proves the real lane takes it.
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut dispatch)
            .await
            .is_err()
    );
    let mut delivery = if worker_tail {
        // Reject the targeted engine transition. The worker's fallback commits
        // Failed and parks this Goal-only original for its native tail delivery.
        rusqlite::Connection::open(directory.path().join("control_plane.db"))
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fixture_park_goal_finish_error
                 BEFORE UPDATE OF status ON tasks
                 WHEN NEW.kind = 'goal' AND NEW.error = 'parent_operation_failed'
                 BEGIN SELECT RAISE(ABORT, 'scripted Goal finish failure'); END;",
            )
            .unwrap();
        None
    } else {
        message.content = "/goal pause now".into();
        let pause = goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
        if ack_failure {
            Some(zeroclaw_spawn::spawn!(async move {
                pause.response_delivery_failed().await.unwrap();
            }))
        } else {
            let pause_channel = channel.clone();
            let pause_message = message.clone();
            Some(zeroclaw_spawn::spawn!(async move {
                goal_execution::deliver_matrix_goal_command(
                    pause,
                    Some(pause_channel),
                    pause_message,
                )
                .await;
            }))
        }
    };
    let failure = channel_runtime_cli_string(if worker_tail {
        "goal-mode-failure-executor"
    } else {
        "goal-mode-failure-outcome-unknown"
    });
    let queued_reply = render_goal_response(&GoalResponse::Queued(GoalQueuedControl::Start));
    let mut successor_delivery = None;
    let mut delivery_waited_for_ordinary = false;
    let mut predecessor_diagnostic = String::new();
    let outcome = async {
        let pending = channel_runtime_cli_string("goal-mode-pause-pending");
        if !worker_tail && !ack_failure {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|row| row.content == pending)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        provider.goal.first_release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), provider.ordinary_entered.notified())
            .await
            .expect("the ordinary lane must hold the foreground before failure delivery");
        tokio::time::timeout(Duration::from_secs(2), async {
            while registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap()
                .status
                != TaskStatus::Failed
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let failed = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        let goal = registry
            .get_goal_task(&predecessor_id)
            .await
            .unwrap()
            .unwrap();
        let reason = registry
            .terminal_reason_for_session_goal(&predecessor_id, &key)
            .await
            .unwrap();
        let sent = channel.sent_messages.lock().await;
        let diagnostic = format!(
            "case={case:?}, task={}, epoch={}, status={:?}, accounting={:?}, pending={:?}/{:?}, reason={reason:?}, sent={:?}",
            failed.id,
            failed.execution_epoch,
            failed.status,
            goal.accounting_state,
            goal.pending_call_id,
            goal.pending_call_epoch,
            sent.iter()
                .map(|row| row.content.as_str())
                .collect::<Vec<_>>()
        );
        predecessor_diagnostic = diagnostic.clone();
        assert_eq!(failed.id, predecessor_id, "{diagnostic}");
        assert_eq!(
            failed.execution_epoch,
            predecessor_epoch + if worker_tail { 1 } else { 2 },
            "{diagnostic}"
        );
        assert_eq!(
            goal.accounting_state,
            if worker_tail {
                zeroclaw_goal::GoalAccountingState::Complete
            } else {
                zeroclaw_goal::GoalAccountingState::OutcomeUnknown
            },
            "{diagnostic}"
        );
        assert!(
            goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none(),
            "{diagnostic}"
        );
        assert_eq!(
            reason.as_deref(),
            Some(if worker_tail {
                "executor_failed"
            } else {
                "accounting_outcome_unknown"
            }),
            "{diagnostic}"
        );
        assert_failed_predecessor_ledger(
            directory.path(),
            &tracker,
            &predecessor_id,
            worker_tail,
            usize::from(worker_tail),
        );
        assert!(
            context
                .session_store
                .as_ref()
                .unwrap()
                .load(&key)
                .iter()
                .all(|row| row.role != "assistant"),
            "no failed parent candidate append; {diagnostic}"
        );
        assert!(
            !sent.iter().any(|row| row
                .content
                .contains("Goal failure lost its execution fence")
                || row.content.contains("scripted Goal finish failure")),
            "Goal-only aggregate must wait for ordinary lane; {diagnostic}"
        );
        drop(sent);
        if ack_failure {
            // Native publication may wait for the ordinary lane, but must not
            // keep the conversation's independent command authority hostage.
            let command = foreground::goal_command_lock(&context.persist_locks, &key);
            let guard = command
                .try_lock_owned()
                .expect("acknowledgement-failure delivery must release Goal command authority");
            drop(guard);
            let status = tokio::time::timeout(
                Duration::from_secs(1),
                goal_execution::submit_matrix_goal(
                    context.clone(),
                    key.clone(),
                    message.clone(),
                    GoalCommand::Status,
                ),
            )
            .await
            .expect("Status must answer while failure delivery waits")
            .unwrap();
            assert!(matches!(status.response(), GoalResponse::Terminal(_)));
            drop(status);
        }
        // Observe without polling the JoinHandle: an unfixed delivery may
        // already have returned, but cleanup must still join it exactly once.
        if let Some(delivery) = delivery.as_ref() {
            delivery_waited_for_ordinary = tokio::time::timeout(Duration::from_millis(20), async {
                while !delivery.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err();
        }
        assert!(
            !channel
                .sent_messages
                .lock()
                .await
                .iter()
                .any(|row| row.content == failure)
        );
        let mut unrelated = matrix_msg("@fixture:example.test");
        unrelated.id = "fixture-unrelated".into();
        unrelated.reply_target = "!other-fixture:example.test".into();
        unrelated.content = "fixture-unrelated-conversation".into();
        sender.send(unrelated).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            provider.unrelated_entered.notified(),
        )
        .await
        .expect("another conversation must progress while delivery waits");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !channel
                .sent_messages
                .lock()
                .await
                .iter()
                .any(|row| row.content == "unrelated conversation finished")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the unrelated reply must be delivered while the ordinary lane remains held");
        if reset_session {
            let mut reset = message.clone();
            reset.id = "fixture-reset-after-failure".into();
            reset.content = "/new".into();
            sender.send(reset).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_some()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect(
                "reset admission must withdraw publication without waiting for the ordinary lane",
            );
            let replies = channel.sent_messages.lock().await;
            assert!(!replies.iter().any(|row| row.content == failure));
            assert!(!replies.iter().any(|row| {
                row.content == channel_runtime_cli_string("channel-runtime-new-session")
            }));
        }
        if start_successor {
            message.content = "/goal start --unlimited bounded successor".into();
            let admission = goal_execution::submit_matrix_goal(
                context.clone(),
                key.clone(),
                message.clone(),
                GoalCommand::Start {
                    budget: GoalBudgetSelection::Unlimited,
                    objective: "bounded successor".into(),
                },
            )
            .await
            .unwrap();
            assert_eq!(
                admission.response(),
                &GoalResponse::Queued(GoalQueuedControl::Start)
            );
            let successor_channel = channel.clone();
            let successor_message = message.clone();
            successor_delivery = Some(zeroclaw_spawn::spawn!(async move {
                goal_execution::deliver_matrix_goal_command(
                    admission,
                    Some(successor_channel),
                    successor_message,
                )
                .await;
            }));
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|row| row.content == queued_reply)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("queued Start acknowledgement must not wait for ordinary work");
            assert_eq!(
                provider
                    .goal
                    .calls
                    .load(std::sync::atomic::Ordering::SeqCst),
                1
            );
        }
    };
    let result = std::panic::AssertUnwindSafe(outcome).catch_unwind().await;
    provider.goal.first_release.notify_one();
    provider.ordinary_release.notify_one();
    if let Some(delivery) = delivery.as_mut() {
        tokio::time::timeout(Duration::from_secs(3), delivery)
            .await
            .unwrap()
            .unwrap();
    }
    if ack_failure && result.is_ok() {
        let failed = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(
            registry
                .terminal_reason_for_session_goal(&failed.id, &key)
                .await
                .unwrap()
                .as_deref(),
            Some("accounting_outcome_unknown")
        );
        assert!(
            foreground::goal_supervisor_slot(&context.persist_locks, &key)
                .lock()
                .await
                .is_none()
        );
    }
    if let Some(mut successor) = successor_delivery {
        tokio::time::timeout(Duration::from_secs(3), &mut successor)
            .await
            .unwrap()
            .unwrap();
    }
    let successor_admission = if start_successor && result.is_ok() {
        tokio::time::timeout(Duration::from_secs(2), async {
            while provider
                .goal
                .calls
                .load(std::sync::atomic::Ordering::SeqCst)
                < 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await
    } else {
        Ok(())
    };
    let successor_task = if start_successor && result.is_ok() && successor_admission.is_ok() {
        Some(registry.current_goal_for_session(&key).await)
    } else {
        None
    };
    drop(sender);
    let disposing = supervisor.clone();
    let disposing_key = key.clone();
    let dispose =
        zeroclaw_spawn::spawn!(async move { disposing.dispose_session(&disposing_key).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .is_some_and(|task| task.status == TaskStatus::Running)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dispose must fence a successor before releasing its admitted call");
    provider.goal.successor_release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), dispose)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), &mut dispatch)
        .await
        .unwrap()
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    successor_admission.expect("the successor must reach its provider before fixture disposal");
    let total_attempts = usize::from(worker_tail) + usize::from(start_successor);
    assert_failed_predecessor_ledger(
        directory.path(),
        &tracker,
        &predecessor_id,
        worker_tail,
        total_attempts,
    );
    if let Some(successor) = successor_task {
        let successor = successor.unwrap().unwrap();
        assert_ne!(
            successor.id, predecessor_id,
            "queued Start must create a distinct Goal"
        );
        assert_eq!(successor.status, TaskStatus::Running);
        let successor_id = successor.id;
        assert_failed_predecessor_ledger(
            directory.path(),
            &tracker,
            &successor_id,
            true,
            total_attempts,
        );
        assert!(
            registry
                .get_goal_task(&successor_id)
                .await
                .unwrap()
                .is_none(),
            "hard disposal removes control state, not its canonical ledger rows"
        );
    }
    let replies = channel.sent_messages.lock().await;
    let diagnostic = format!(
        "{predecessor_diagnostic}, final sent={:?}",
        replies
            .iter()
            .map(|row| row.content.as_str())
            .collect::<Vec<_>>()
    );
    let semantic_cause = "provider completed without final text or tool calls";
    let original_cause = if worker_tail {
        semantic_cause
    } else {
        "scripted parent failure"
    };
    let original_rows: Vec<_> = replies
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row.content.starts_with("⚠️ Error: ")
                && row.content.contains(original_cause)
                && !row
                    .content
                    .contains("Goal failure lost its execution fence")
                && !row.content.contains("scripted Goal finish failure")
        })
        .collect();
    assert_eq!(
        original_rows.len(),
        1,
        "one ordinary native error; {diagnostic}"
    );
    let (original, original_row) = original_rows[0];
    assert_eq!(
        original_row.content.matches(original_cause).count(),
        1,
        "{diagnostic}"
    );
    let semantic_heading = zeroclaw_runtime::agent::terminal_completion_error_message(
        &anyhow::Error::new(zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion),
        None,
    )
    .unwrap();
    if worker_tail {
        assert!(
            original_row
                .content
                .starts_with(&format!("⚠️ Error: {semantic_heading}")),
            "{diagnostic}"
        );
    } else {
        assert!(original_row.content.contains("Goal accounting is outcome-unknown after a durable usage settlement; no further Goal model call will be admitted: accounting issue for actual provider `test-provider`, model `test-model`: usage outcome unknown"), "{diagnostic}");
    }
    let ordinary = replies
        .iter()
        .position(|row| row.content == "ordinary lane finished")
        .unwrap_or_else(|| panic!("ordinary lane must complete; {diagnostic}"));
    assert_eq!(
        replies
            .iter()
            .filter(|row| row.content == "ordinary lane finished")
            .count(),
        1,
        "{diagnostic}"
    );
    assert_eq!(
        replies
            .iter()
            .filter(|row| row.content == "unrelated conversation finished")
            .count(),
        1,
        "{diagnostic}"
    );
    assert_eq!(
        replies
            .iter()
            .filter(|row| row.content.starts_with("⚠️ Error: "))
            .count(),
        if reset_session { 1 } else { 2 },
        "one original and only the owed transition aggregate; {diagnostic}"
    );
    if reset_session {
        let reset = replies
            .iter()
            .position(|row| {
                row.content == channel_runtime_cli_string("channel-runtime-new-session")
            })
            .expect("reset must follow the earlier ordinary turn");
        assert!(original < ordinary && ordinary < reset, "{diagnostic}");
        assert!(
            !replies.iter().any(|row| row.content == failure),
            "{diagnostic}"
        );
        assert!(
            !replies
                .iter()
                .any(|row| row.content.contains("scripted Goal finish failure")),
            "withdrawn aggregate must not publish; {diagnostic}"
        );
        assert_eq!(
            provider
                .goal
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .is_none()
        );
        return;
    }
    let notice = replies
        .iter()
        .position(|row| row.content == failure)
        .unwrap_or_else(|| {
            panic!("failure notice must survive the closed admission checkpoint; {diagnostic}")
        });
    let transition_cause = if worker_tail {
        "scripted Goal finish failure"
    } else {
        "Goal failure lost its execution fence"
    };
    let parked: Vec<_> = replies
        .iter()
        .enumerate()
        .filter(|(_, row)| row.content.contains(transition_cause))
        .collect();
    assert_eq!(parked.len(), 1, "one parked native aggregate; {diagnostic}");
    let (parked_index, parked_row) = parked[0];
    assert!(parked_row.content.starts_with("⚠️ Error: "), "{diagnostic}");
    assert_eq!(
        parked_row.content.matches(transition_cause).count(),
        1,
        "{diagnostic}"
    );
    assert_eq!(
        parked_row.content.matches(original_cause).count(),
        1,
        "{diagnostic}"
    );
    assert!(
        original < ordinary && ordinary < parked_index && parked_index < notice,
        "{diagnostic}"
    );
    if worker_tail {
        assert!(
            parked_row
                .content
                .starts_with(&format!("⚠️ Error: {semantic_heading}")),
            "{diagnostic}"
        );
        assert_eq!(
            parked_row
                .content
                .matches("Goal parent operation failed")
                .count(),
            1,
            "storage aggregate must retain parent and semantic causes; {diagnostic}"
        );
    } else if ack_failure {
        assert!(delivery_waited_for_ordinary);
        assert!(
            parked_row
                .content
                .contains(&original_row.content["⚠️ Error: ".len()..]),
            "lost-fence aggregate must retain the complete original diagnostic; {diagnostic}"
        );
        assert!(!replies.iter().any(|row| {
            row.content
                .starts_with(&channel_runtime_cli_string("goal-mode-terminal"))
        }));
    } else {
        assert!(
            delivery_waited_for_ordinary,
            "failure delivery must wait for the ordinary lane"
        );
        let late = replies
            .iter()
            .position(|row| {
                row.content
                    .starts_with(&channel_runtime_cli_string("goal-mode-terminal"))
            })
            .unwrap();
        let ack = replies
            .iter()
            .position(|row| row.content == channel_runtime_cli_string("goal-mode-pause-pending"))
            .unwrap();
        assert!(ack < original && original < ordinary && ordinary < notice && notice < late);
        assert!(
            parked_row
                .content
                .contains(&original_row.content["⚠️ Error: ".len()..]),
            "lost-fence aggregate must retain the complete original diagnostic; {diagnostic}"
        );
    }
    assert_eq!(
        replies.iter().filter(|row| row.content == failure).count(),
        1
    );
    assert!(
        replies
            .iter()
            .any(|row| row.content == "unrelated conversation finished")
    );
    if start_successor {
        let queued = replies
            .iter()
            .position(|row| row.content == queued_reply)
            .unwrap();
        assert!(original < queued && queued < ordinary);
        let started = channel_runtime_cli_string("goal-mode-started");
        let starts: Vec<_> = replies
            .iter()
            .enumerate()
            .filter(|(_, row)| row.content.starts_with(&started))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(starts.len(), 2);
        assert!(notice < starts[1]);
        assert_eq!(
            provider
                .goal
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    } else {
        assert_eq!(
            provider
                .goal
                .calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }
}

#[tokio::test]
async fn matrix_goal_failed_predecessor_max_epoch_status_preserves_terminal_record() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_failed_predecessor_max_epoch_status_preserves_terminal_record
    ))
    .await
    {
        return;
    }
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_goal::{GoalExecutionScope, GoalResponse, GoalRuntime};
    use zeroclaw_runtime::{
        control_plane::{ControlPlaneHandle, TaskStatus},
        cost::CostTracker,
        goal_accounting::{ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory},
    };

    let directory = tempfile::tempdir().unwrap();
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GoalFailedPredecessorProvider::default());
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider = "custom.fixture".into();
    config.providers.models.ensure("custom", "fixture").unwrap();
    config.reliability.provider_retries = 0;
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    let message = matrix_msg("@fixture:example.test");
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle.clone());
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            directory.path(),
        )
        .unwrap(),
    );
    let accounting = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            registry.clone(),
            tracker.clone(),
            context.agent_alias.to_string(),
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor = handle
        .goal_execution_restart()
        .new_supervisor(Arc::new(
            GoalRuntime::new(registry.clone()).execution_engine(accounting),
        ))
        .await;
    *foreground::goal_supervisor_slot(&context.persist_locks, &key)
        .lock()
        .await = Some(supervisor.clone());
    let initial = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Start {
            budget: GoalBudgetSelection::Unlimited,
            objective: "bounded predecessor repair".into(),
        },
    )
    .await
    .unwrap();
    let GoalResponse::Started(started) = initial.response() else {
        panic!("fixture must start directly");
    };
    let scope = GoalExecutionScope::new(&started.task_id, &key, started.execution_epoch).unwrap();
    // Establish the exact operator-pause winner before dropping unlaunched
    // custody; cancellation cleanup must preserve that canonical fence.
    assert_eq!(
        registry
            .pause_session_goal(
                scope.task_id(),
                &key,
                scope.execution_epoch(),
                zeroclaw_goal::GoalPauseState {
                    reason: zeroclaw_goal::GoalPauseReason::OperatorPaused,
                    description: None,
                    blockers: Vec::new(),
                },
            )
            .await
            .unwrap(),
        zeroclaw_goal::GoalTransitionResult::Applied,
    );
    drop(initial);
    let mut paused = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Pause { immediate: true },
    )
    .await
    .unwrap();
    assert!(
        matches!(paused.response(), GoalResponse::AlreadyPaused(projection)
        if projection.pause_reason == Some(zeroclaw_goal::GoalPauseReason::OperatorPaused))
    );
    paused.after_response().await.unwrap();
    drop(paused);
    assert!(!supervisor.owns_scope(&scope).await);
    let connection = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
    assert_eq!(connection.execute(
        "UPDATE tasks SET execution_epoch = 9223372036854775806 WHERE id = ?1 AND kind = 'goal'",
        [&scope.task_id()],
    ).unwrap(), 1);
    drop(connection);
    let seeded = registry
        .current_goal_for_session(&key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seeded.status, TaskStatus::Paused);
    assert_eq!(seeded.execution_epoch, i64::MAX - 1);
    let status = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Status,
    )
    .await
    .unwrap();
    assert!(matches!(status.response(), GoalResponse::Status(projection)
        if projection.resumable
            && projection.pause_reason == Some(zeroclaw_goal::GoalPauseReason::OperatorPaused)));
    drop(status);
    let mut resumed = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Resume { response: None },
    )
    .await
    .unwrap();
    let GoalResponse::Resumed(projection) = resumed.response() else {
        panic!("seeded predecessor must resume directly");
    };
    assert_eq!(projection.execution_epoch, i64::MAX);
    let running = registry
        .current_goal_for_session(&key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(running.status, TaskStatus::Running);
    assert_eq!(running.execution_epoch, i64::MAX);
    resumed.after_response().await.unwrap();
    let notice =
        zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-failure-outcome-unknown");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count = channel
                .sent_messages
                .lock()
                .await
                .iter()
                .filter(|row| row.content == notice)
                .count();
            assert!(count <= 1);
            if count == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed worker must publish exactly one notice");
    let predecessor = registry
        .current_goal_for_session(&key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(predecessor.status, TaskStatus::Failed);
    assert_eq!(predecessor.id, scope.task_id());
    assert_eq!(predecessor.execution_epoch, i64::MAX);
    let goal = registry
        .get_goal_task(&predecessor.id)
        .await
        .unwrap()
        .unwrap();
    let reason = registry
        .terminal_reason_for_session_goal(&predecessor.id, &key)
        .await
        .unwrap();
    let sent = channel.sent_messages.lock().await;
    let diagnostic = format!(
        "mode=BareError/max-epoch, task={}, epoch={}, status={:?}, accounting={:?}, pending={:?}/{:?}, reason={reason:?}, sent={:?}",
        predecessor.id,
        predecessor.execution_epoch,
        predecessor.status,
        goal.accounting_state,
        goal.pending_call_id,
        goal.pending_call_epoch,
        sent.iter()
            .map(|row| row.content.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        goal.accounting_state,
        zeroclaw_goal::GoalAccountingState::OutcomeUnknown,
        "{diagnostic}"
    );
    assert!(
        goal.pending_call_id.is_none() && goal.pending_call_epoch.is_none(),
        "{diagnostic}"
    );
    assert_failed_predecessor_ledger(directory.path(), &tracker, &predecessor.id, false, 0);
    assert!(
        context
            .session_store
            .as_ref()
            .unwrap()
            .load(&key)
            .iter()
            .all(|row| row.role != "assistant"),
        "no failed parent candidate append; {diagnostic}"
    );
    let originals: Vec<_> = sent
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row.content.starts_with("⚠️ Error: ") && row.content.contains("scripted parent failure")
        })
        .collect();
    assert_eq!(originals.len(), 1, "{diagnostic}");
    assert_eq!(
        sent.iter()
            .filter(|row| row.content.starts_with("⚠️ Error: "))
            .count(),
        1,
        "{diagnostic}"
    );
    assert!(originals[0].1.content.contains("Goal accounting is outcome-unknown after a durable usage settlement; no further Goal model call will be admitted: accounting issue for actual provider `test-provider`, model `test-model`: usage outcome unknown"), "{diagnostic}");
    let notice_index = sent.iter().position(|row| row.content == notice).unwrap();
    assert!(originals[0].0 < notice_index, "{diagnostic}");
    drop(sent);
    assert_eq!(reason.as_deref(), Some("accounting_outcome_unknown"));
    let next = tokio::time::timeout(
        Duration::from_secs(5),
        goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message,
            GoalCommand::Status,
        ),
    )
    .await
    .expect("Status must not wait on the predecessor drain")
    .unwrap_or_else(|error| panic!("Status must not inherit the predecessor failure: {error:#}"));
    assert!(matches!(next.response(), GoalResponse::Terminal(projection)
        if projection.task_id == predecessor.id && projection.status == TaskStatus::Failed));
    assert_eq!(
        serde_json::to_value(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(&predecessor).unwrap()
    );
    assert_eq!(
        registry
            .get_goal_task(&predecessor.id)
            .await
            .unwrap()
            .unwrap(),
        goal,
        "Status must preserve the exact settled accounting record; {diagnostic}"
    );
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(next);
    supervisor.dispose_session(&key).await.unwrap();
    let sent = channel.sent_messages.lock().await;
    assert_eq!(sent.iter().filter(|row| row.content == notice).count(), 1);
    assert!(!sent.iter().any(|row| row.content
        == zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-command-failed")));
}

#[tokio::test]
async fn matrix_goal_failed_predecessor_status_preserves_terminal_record() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_failed_predecessor_status_preserves_terminal_record
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_failed_predecessor_control(false, false).await;
}

#[tokio::test]
async fn matrix_goal_failed_predecessor_start_consumes_completed_worker() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_failed_predecessor_start_consumes_completed_worker
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_failed_predecessor_control(true, false).await;
}

#[tokio::test]
async fn matrix_goal_failed_predecessor_max_epoch_start_consumes_completed_worker() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_failed_predecessor_max_epoch_start_consumes_completed_worker
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_failed_predecessor_control(true, true).await;
}

async fn assert_matrix_goal_failed_predecessor_control(replace: bool, exhausted_epoch: bool) {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_goal::{GoalExecutionScope, GoalResponse, GoalRuntime};
    use zeroclaw_runtime::{
        control_plane::{ControlPlaneHandle, TaskStatus},
        cost::CostTracker,
        goal_accounting::{ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory},
    };

    let directory = tempfile::tempdir().unwrap();
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GoalFailedPredecessorProvider::default());
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider = "custom.fixture".into();
    config.providers.models.ensure("custom", "fixture").unwrap();
    config.reliability.provider_retries = 0;
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    let message = matrix_msg("@fixture:example.test");
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle.clone());
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            directory.path(),
        )
        .unwrap(),
    );
    let accounting = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            registry.clone(),
            tracker,
            context.agent_alias.to_string(),
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let supervisor = handle
        .goal_execution_restart()
        .new_supervisor(Arc::new(
            GoalRuntime::new(registry.clone()).execution_engine(accounting),
        ))
        .await;
    *foreground::goal_supervisor_slot(&context.persist_locks, &key)
        .lock()
        .await = Some(supervisor.clone());
    let mut initial = goal_execution::submit_matrix_goal(
        context.clone(),
        key.clone(),
        message.clone(),
        GoalCommand::Start {
            budget: GoalBudgetSelection::Unlimited,
            objective: "bounded predecessor repair".into(),
        },
    )
    .await
    .unwrap();
    let GoalResponse::Started(started) = initial.response() else {
        panic!("fixture must start directly");
    };
    let mut scope =
        GoalExecutionScope::new(&started.task_id, &key, started.execution_epoch).unwrap();
    if exhausted_epoch {
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        // Establish the exact operator-pause winner before dropping unlaunched
        // custody; cancellation cleanup must preserve that canonical fence.
        assert_eq!(
            registry
                .pause_session_goal(
                    scope.task_id(),
                    &key,
                    scope.execution_epoch(),
                    zeroclaw_goal::GoalPauseState {
                        reason: zeroclaw_goal::GoalPauseReason::OperatorPaused,
                        description: None,
                        blockers: Vec::new(),
                    },
                )
                .await
                .unwrap(),
            zeroclaw_goal::GoalTransitionResult::Applied,
        );
        drop(initial);
        let mut paused = goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message.clone(),
            GoalCommand::Pause { immediate: true },
        )
        .await
        .unwrap();
        assert!(
            matches!(paused.response(), GoalResponse::AlreadyPaused(projection)
            if projection.pause_reason == Some(zeroclaw_goal::GoalPauseReason::OperatorPaused))
        );
        paused.after_response().await.unwrap();
        drop(paused);
        assert!(!supervisor.owns_scope(&scope).await);
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let connection =
            rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
        assert_eq!(
            connection
                .execute(
                    "UPDATE tasks SET execution_epoch = 9223372036854775806 WHERE id = ?1 AND kind = 'goal'",
                    [&scope.task_id()],
                )
                .unwrap(),
            1
        );
        drop(connection);
        let seeded = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(seeded.status, TaskStatus::Paused);
        assert_eq!(seeded.execution_epoch, i64::MAX - 1);
        let status = goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message.clone(),
            GoalCommand::Status,
        )
        .await
        .unwrap();
        assert!(matches!(status.response(), GoalResponse::Status(projection)
            if projection.resumable
                && projection.pause_reason == Some(zeroclaw_goal::GoalPauseReason::OperatorPaused)));
        drop(status);
        initial = goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message.clone(),
            GoalCommand::Resume { response: None },
        )
        .await
        .unwrap();
        let GoalResponse::Resumed(resumed) = initial.response() else {
            panic!("seeded predecessor must resume directly");
        };
        assert_eq!(resumed.execution_epoch, i64::MAX);
        scope = GoalExecutionScope::new(&resumed.task_id, &key, resumed.execution_epoch).unwrap();
        let running = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(running.status, TaskStatus::Running);
        assert_eq!(running.execution_epoch, i64::MAX);
    }
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    initial.after_response().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !supervisor.worker_completed_for_session(&key).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed worker must publish and complete");
    let predecessor = registry
        .current_goal_for_session(&key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(predecessor.status, TaskStatus::Failed);
    if exhausted_epoch {
        assert_eq!(predecessor.execution_epoch, i64::MAX);
    } else {
        assert!(predecessor.execution_epoch > scope.execution_epoch());
    }
    assert_eq!(
        supervisor
            .owns_scope(
                &GoalExecutionScope::new(&predecessor.id, &key, predecessor.execution_epoch,)
                    .unwrap()
            )
            .await,
        exhausted_epoch
    );
    let terminal_reason = registry
        .terminal_reason_for_session_goal(&predecessor.id, &key)
        .await
        .unwrap();
    let captured_messages = channel.sent_messages.lock().await.clone();
    let provider_calls = provider.calls.load(std::sync::atomic::Ordering::SeqCst);
    let diagnostic = format!(
        "predecessor={predecessor:?}; terminal_reason={terminal_reason:?}; provider_calls={provider_calls}; sent_messages={captured_messages:?}"
    );
    assert_eq!(
        terminal_reason.as_deref(),
        Some("accounting_outcome_unknown"),
        "{diagnostic}"
    );
    let notice =
        zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-failure-outcome-unknown");
    assert_eq!(
        captured_messages
            .iter()
            .filter(|row| row.content == notice)
            .count(),
        1,
        "{diagnostic}"
    );
    let original_indices: Vec<_> = captured_messages
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row.content.starts_with("⚠️ Error: ") && row.content.contains("scripted parent failure")
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(original_indices.len(), 1, "{diagnostic}");
    assert_eq!(
        captured_messages
            .iter()
            .filter(|row| row.content.starts_with("⚠️ Error: "))
            .count(),
        1,
        "{diagnostic}"
    );
    let notice_index = captured_messages
        .iter()
        .position(|row| row.content == notice)
        .unwrap();
    assert!(original_indices[0] < notice_index, "{diagnostic}");
    let command = if replace {
        GoalCommand::Start {
            budget: GoalBudgetSelection::Unlimited,
            objective: "bounded successor repair".into(),
        }
    } else {
        GoalCommand::Status
    };
    let mut next = tokio::time::timeout(
        Duration::from_secs(5),
        goal_execution::submit_matrix_goal(context.clone(), key.clone(), message, command),
    )
    .await
    .expect("next command must not wait on the predecessor drain")
    .unwrap_or_else(|error| {
        panic!("next command must not inherit the predecessor failure: {error:#}")
    });
    if replace {
        let GoalResponse::Started(started) = next.response() else {
            panic!("completed predecessor must allow a direct Start, not Queued");
        };
        assert_ne!(started.task_id, predecessor.id);
        next.after_response().await.unwrap();
        assert!(!supervisor.owns_scope(&scope).await);
        tokio::time::timeout(Duration::from_secs(5), async {
            while provider.calls.load(std::sync::atomic::Ordering::SeqCst) != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let disposing = supervisor.clone();
        let disposing_key = key.clone();
        let cleanup =
            zeroclaw_spawn::spawn!(async move { disposing.dispose_session(&disposing_key).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .is_some_and(|task| task.status != TaskStatus::Cancelled)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("disposal must fence the successor before releasing its model call");
        provider.successor_release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), cleanup)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    } else {
        assert!(matches!(next.response(), GoalResponse::Terminal(projection)
            if projection.task_id == predecessor.id && projection.status == TaskStatus::Failed));
        assert_eq!(
            serde_json::to_value(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(&predecessor).unwrap()
        );
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        supervisor.dispose_session(&key).await.unwrap();
    }
    let sent = channel.sent_messages.lock().await;
    assert_eq!(sent.iter().filter(|row| row.content == notice).count(), 1);
    assert_eq!(
        sent.iter()
            .filter(|row| row.content.starts_with("⚠️ Error: "))
            .count(),
        1,
        "{sent:?}"
    );
    assert!(!sent.iter().any(|row| row.content
        == zeroclaw_runtime::i18n::get_required_cli_string("goal-mode-command-failed")));
}

#[derive(Default)]
struct GoalIngressBoundaryProvider {
    requests: std::sync::Mutex<Vec<Vec<zeroclaw_api::model_provider::ChatMessage>>>,
    ordinary_release: tokio::sync::Notify,
    goal_release: tokio::sync::Notify,
    resumed_release: tokio::sync::Notify,
}

impl zeroclaw_api::attribution::Attributable for GoalIngressBoundaryProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Custom,
        ))
    }

    fn alias(&self) -> &str {
        "goal-ingress-boundary-fixture"
    }
}

#[async_trait::async_trait]
impl ModelProvider for GoalIngressBoundaryProvider {
    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn chat_with_system(
        &self,
        _: Option<&str>,
        message: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        if message.starts_with("Decide whether the assistant should send") {
            return Ok("REPLY".into());
        }
        anyhow::bail!("fixture requires the ordinary structured model boundary")
    }

    async fn chat(
        &self,
        request: zeroclaw_api::model_provider::ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<zeroclaw_api::model_provider::ChatResponse> {
        let call = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.messages.to_vec());
            requests.len()
        };
        let text = match call {
            1 => {
                self.ordinary_release.notified().await;
                "ordinary boundary candidate"
            }
            2 => {
                self.goal_release.notified().await;
                "Goal boundary candidate"
            }
            3 => {
                self.resumed_release.notified().await;
                "resumed boundary candidate"
            }
            _ => anyhow::bail!("cancelled fixture must not admit a verifier or successor"),
        };
        Ok(zeroclaw_api::model_provider::ChatResponse {
            text: Some(text.into()),
            tool_calls: Vec::new(),
            usage: Some(zeroclaw_api::model_provider::TokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(2),
                ..Default::default()
            }),
            reasoning_content: None,
        })
    }
}

struct HeldGoalLifecycleChannel {
    inner: SendMessageRecordingChannel,
    send_count: std::sync::atomic::AtomicUsize,
    hold_at: usize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl zeroclaw_api::attribution::Attributable for HeldGoalLifecycleChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Matrix)
    }

    fn alias(&self) -> &str {
        "held-goal-lifecycle-fixture"
    }
}

#[async_trait::async_trait]
impl Channel for HeldGoalLifecycleChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let ordinal = self
            .send_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if ordinal == self.hold_at {
            // Hold the native send before it is observable, not the queued
            // acknowledgement or a synthetic pre-commit boundary.
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.send(message).await
    }

    async fn listen(&self, _: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn matrix_goal_aborted_direct_launch_notice_preserves_resumable_identity() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_aborted_direct_launch_notice_preserves_resumable_identity
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_launch_notice_settlement(false, false, GoalLaunchNoticeEnding::Abort).await;
}

#[tokio::test]
async fn matrix_goal_aborted_direct_resume_notice_preserves_resumable_identity() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_aborted_direct_resume_notice_preserves_resumable_identity
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_launch_notice_settlement(true, false, GoalLaunchNoticeEnding::Abort).await;
}

#[tokio::test]
async fn matrix_goal_aborted_queued_launch_notice_preserves_resumable_identity() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_aborted_queued_launch_notice_preserves_resumable_identity
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_launch_notice_settlement(false, true, GoalLaunchNoticeEnding::Abort).await;
}

#[tokio::test]
async fn matrix_goal_aborted_queued_resume_notice_preserves_resumable_identity() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_aborted_queued_resume_notice_preserves_resumable_identity
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_launch_notice_settlement(true, true, GoalLaunchNoticeEnding::Abort).await;
}

#[tokio::test]
async fn matrix_goal_reported_direct_launch_failure_has_no_duplicate_command_error() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_reported_direct_launch_failure_has_no_duplicate_command_error
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_launch_notice_settlement(
        false,
        false,
        GoalLaunchNoticeEnding::ReportedFailure,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_reported_queued_resume_failure_has_no_duplicate_command_error() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_reported_queued_resume_failure_has_no_duplicate_command_error
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_launch_notice_settlement(
        true,
        true,
        GoalLaunchNoticeEnding::ReportedFailure,
    )
    .await;
}

enum GoalLaunchNoticeEnding {
    Abort,
    ReportedFailure,
    StaleFailure,
    ReconciliationFailure,
}

#[tokio::test]
async fn matrix_goal_stale_launch_failure_reports_only_command_error() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stale_launch_failure_reports_only_command_error
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_launch_notice_settlement(false, false, GoalLaunchNoticeEnding::StaleFailure)
        .await;
}

#[tokio::test]
async fn matrix_goal_launch_reconciliation_failure_reports_only_command_error() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_launch_reconciliation_failure_reports_only_command_error
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_launch_notice_settlement(
        false,
        false,
        GoalLaunchNoticeEnding::ReconciliationFailure,
    )
    .await;
}

async fn assert_matrix_goal_launch_notice_settlement(
    resume: bool,
    queued: bool,
    ending: GoalLaunchNoticeEnding,
) {
    use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
    use zeroclaw_goal::{GoalAccountingState, GoalPauseReason, GoalResponse};
    use zeroclaw_runtime::control_plane::{ControlPlaneHandle, TaskStatus};

    let directory = tempfile::tempdir().unwrap();
    let verifier_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .expect(0)
        .mount(&verifier_server)
        .await;
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(HeldGoalLifecycleChannel {
        inner: SendMessageRecordingChannel::matrix(),
        send_count: std::sync::atomic::AtomicUsize::new(0),
        hold_at: if queued { 2 } else { 1 },
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let provider = Arc::new(GoalIngressBoundaryProvider::default());
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider =
        zeroclaw_config::providers::ModelProviderRef::new("openai.fixture");
    let verifier = config.providers.models.ensure("openai", "fixture").unwrap();
    verifier.api_key = Some("synthetic-key".into());
    verifier.uri = Some(verifier_server.uri());
    verifier.kind = Some("openai-compatible".into());
    verifier.wire_api = Some(zeroclaw_config::schema::WireApi::ChatCompletions);
    verifier.model = Some("fixture-verifier".into());
    verifier.live_pricing = false;
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    let message = matrix_msg("@fixture:example.test");
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle.clone());
    // Keep the fixture's real store and ledger co-located rather than sharing
    // a process-global tracker across unrelated temporary test directories.
    let tracker = Arc::new(
        zeroclaw_runtime::cost::CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            directory.path(),
        )
        .unwrap(),
    );
    let accounting =
        Arc::new(
            zeroclaw_runtime::goal_accounting::RuntimeGoalOperationAccountingFactory::new(
                registry.clone(),
                tracker,
                context.agent_alias.to_string(),
                Arc::new(
                    zeroclaw_runtime::goal_accounting::ModelProviderGoalUsagePricer::new(
                        Arc::default(),
                    ),
                ),
            )
            .unwrap(),
        );
    let supervisor = handle
        .goal_execution_restart()
        .new_supervisor(Arc::new(
            zeroclaw_goal::GoalRuntime::new(registry.clone()).execution_engine(accounting),
        ))
        .await;
    let slot = foreground::goal_supervisor_slot(&context.persist_locks, &key);
    *slot.lock().await = Some(supervisor.clone());
    let start = GoalCommand::Start {
        budget: GoalBudgetSelection::Unlimited,
        objective: "finish the bounded lifecycle fixture".into(),
    };
    let predecessor = if resume {
        let initial = goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message.clone(),
            start.clone(),
        )
        .await
        .unwrap();
        initial.response_delivery_failed().await.unwrap();
        *slot.lock().await = Some(supervisor.clone());
        Some(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap(),
        )
    } else {
        None
    };
    let ordinary = if queued {
        Some(
            foreground::foreground_lock(&context.persist_locks, &key)
                .lock_owned()
                .await,
        )
    } else {
        None
    };
    let delivery = if !matches!(ending, GoalLaunchNoticeEnding::Abort) {
        let ingress_context = context.clone();
        let mut ingress_message = message.clone();
        ingress_message.content = if resume {
            "/goal resume"
        } else {
            "/goal start --unlimited finish the bounded lifecycle fixture"
        }
        .into();
        zeroclaw_spawn::spawn!(async move {
            route_matrix_goal_fixture_control(ingress_context, ingress_message).await;
        })
    } else {
        let command = goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message.clone(),
            if resume {
                GoalCommand::Resume { response: None }
            } else {
                start
            },
        )
        .await
        .unwrap();
        assert_eq!(
            matches!(command.response(), GoalResponse::Queued(_)),
            queued
        );
        let output = channel.clone();
        let delivery_message = message.clone();
        zeroclaw_spawn::spawn!(async move {
            goal_execution::deliver_matrix_goal_command(command, Some(output), delivery_message)
                .await;
        })
    };
    if queued {
        tokio::time::timeout(Duration::from_secs(2), async {
            while channel.inner.sent_messages.lock().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(ordinary);
    }
    tokio::time::timeout(Duration::from_secs(2), channel.entered.notified())
        .await
        .unwrap();
    let admitted = registry
        .current_goal_for_session(&key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admitted.status, TaskStatus::Running);
    if let Some(predecessor) = predecessor {
        assert_eq!(admitted.id, predecessor.id);
        assert_eq!(admitted.execution_epoch, predecessor.execution_epoch + 1);
    }
    assert!(provider.requests.lock().unwrap().is_empty());
    if !matches!(ending, GoalLaunchNoticeEnding::Abort) {
        let barrier =
            foreground::install_goal_test_bind_barrier(&context.persist_locks, &key, true);
        channel.release.notify_one();
        let entered =
            tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified()).await;
        let audit = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
        let injected = if entered.is_ok() {
            match ending {
                GoalLaunchNoticeEnding::StaleFailure => {
                    registry
                        .finish_session_goal(
                            &admitted.id,
                            &key,
                            admitted.execution_epoch,
                            TaskStatus::Cancelled,
                            None,
                        )
                        .await
                }
                GoalLaunchNoticeEnding::ReconciliationFailure => audit
                    .execute_batch(
                        "CREATE TRIGGER fixture_refuse_goal_failure
                         BEFORE UPDATE OF status ON tasks
                         WHEN NEW.kind = 'goal' AND NEW.status = 'failed'
                         BEGIN SELECT RAISE(ABORT, 'fixture reconciliation unavailable'); END;",
                    )
                    .map(|()| zeroclaw_goal::GoalTransitionResult::Applied)
                    .map_err(anyhow::Error::from),
                GoalLaunchNoticeEnding::Abort | GoalLaunchNoticeEnding::ReportedFailure => {
                    Ok(zeroclaw_goal::GoalTransitionResult::Applied)
                }
            }
        } else {
            Ok(zeroclaw_goal::GoalTransitionResult::Applied)
        };
        // Release even on a failed observation, so fixture cleanup cannot
        // leave the actual driver blocked at the synthetic fault boundary.
        barrier.release.notify_one();
        entered.expect("launch must rebind after lifecycle delivery");
        assert_eq!(
            injected.unwrap(),
            zeroclaw_goal::GoalTransitionResult::Applied
        );
        tokio::time::timeout(Duration::from_secs(2), delivery)
            .await
            .expect("reported launch failure must settle delivery")
            .unwrap();
        let failed = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.id, admitted.id);
        if matches!(
            ending,
            GoalLaunchNoticeEnding::StaleFailure | GoalLaunchNoticeEnding::ReconciliationFailure
        ) {
            let stale = matches!(ending, GoalLaunchNoticeEnding::StaleFailure);
            assert_eq!(
                failed.status,
                if stale {
                    TaskStatus::Cancelled
                } else {
                    TaskStatus::Running
                }
            );
            assert_eq!(
                failed.execution_epoch,
                admitted.execution_epoch + i64::from(stale)
            );
            assert!(
                registry
                    .terminal_reason_for_session_goal(&failed.id, &key)
                    .await
                    .unwrap()
                    .is_none()
            );
            if !stale {
                audit
                    .execute_batch("DROP TRIGGER fixture_refuse_goal_failure")
                    .unwrap();
            }
            let replies = channel.inner.sent_messages.lock().await;
            let generic = channel_runtime_cli_string("goal-mode-command-failed");
            let notice = channel_runtime_cli_string("goal-mode-failure-executor-start");
            assert_eq!(replies.len(), 2 + usize::from(queued));
            assert!(replies[usize::from(queued)].content.contains(if resume {
                "Goal resumed."
            } else {
                "Goal started."
            }));
            assert_eq!(replies.last().unwrap().content, generic);
            assert_eq!(
                replies
                    .iter()
                    .filter(|reply| reply.content == generic)
                    .count(),
                1
            );
            assert!(replies.iter().all(|reply| reply.content != notice));
            assert!(provider.requests.lock().unwrap().is_empty());
            return;
        }
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(
            registry
                .terminal_reason_for_session_goal(&failed.id, &key)
                .await
                .unwrap()
                .as_deref(),
            Some("executor_start_failed")
        );
        let replies = channel.inner.sent_messages.lock().await;
        assert_eq!(replies.len(), 2 + usize::from(queued));
        assert!(replies[usize::from(queued)].content.contains(if resume {
            "Goal resumed."
        } else {
            "Goal started."
        }));
        assert_eq!(
            replies.last().unwrap().content,
            channel_runtime_cli_string("goal-mode-failure-executor-start")
        );
        let command_failed = channel_runtime_cli_string("goal-mode-command-failed");
        assert!(replies.iter().all(|reply| reply.content != command_failed));
        assert!(provider.requests.lock().unwrap().is_empty());
        return;
    }
    delivery.abort();
    assert!(delivery.await.unwrap_err().is_cancelled());
    let paused = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = registry
                .current_goal_for_session(&key)
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
    .unwrap();
    assert_eq!(paused.id, admitted.id);
    assert_eq!(paused.execution_epoch, admitted.execution_epoch + 1);
    let goal = registry.get_goal_task(&paused.id).await.unwrap().unwrap();
    assert_eq!(goal.pause_reason, Some(GoalPauseReason::ExternalDependency));
    assert_eq!(goal.accounting_state, GoalAccountingState::Complete);
    assert!(goal.pending_call_id.is_none());
    assert!(goal.pending_call_epoch.is_none());
    if queued {
        let audit = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
        let state: String = audit
            .query_row(
                "SELECT state FROM goal_control_intents WHERE session_id = ?1",
                [&key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "applied",
            "notice cancellation must not rewrite the committed audit"
        );
    }
    assert!(provider.requests.lock().unwrap().is_empty());
    assert_eq!(
        channel.inner.sent_messages.lock().await.len(),
        usize::from(queued)
    );
    assert!(
        foreground::foreground_lock(&context.persist_locks, &key)
            .try_lock()
            .is_ok()
    );
    assert!(
        foreground::goal_command_lock(&context.persist_locks, &key)
            .try_lock()
            .is_ok()
    );

    let recovery = goal_execution::submit_matrix_goal(
        context,
        key.clone(),
        message,
        GoalCommand::Resume { response: None },
    )
    .await
    .unwrap();
    let GoalResponse::Resumed(recovered) = recovery.response() else {
        panic!("explicit authorized recovery must resume the same Goal");
    };
    assert_eq!(recovered.task_id, paused.id);
    assert_eq!(recovered.execution_epoch, paused.execution_epoch + 1);
    // Do not launch model work merely to test recoverable admission.
    recovery.response_delivery_failed().await.unwrap();
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn matrix_goal_start_is_queued_through_actual_ingress_during_ordinary_turn() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_start_is_queued_through_actual_ingress_during_ordinary_turn
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(GoalIngressAdmission::Start, GoalIngressEnding::Cancel)
        .await;
}

#[tokio::test]
async fn matrix_goal_resume_is_queued_through_actual_ingress_with_multiline_response() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_resume_is_queued_through_actual_ingress_with_multiline_response
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Resume(Some("First fixture answer\nSecond fixture line.")),
        GoalIngressEnding::Cancel,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_plain_resume_waits_for_ordinary_turn_without_fabricating_response() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_plain_resume_waits_for_ordinary_turn_without_fabricating_response
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Resume(None),
        GoalIngressEnding::Cancel,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_idle_final_reply_retires_supervisor_through_actual_ingress() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_idle_final_reply_retires_supervisor_through_actual_ingress
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Resume(None),
        GoalIngressEnding::IdleRetire,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_admission_at_bind_preserves_custody_through_actual_ingress() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_admission_at_bind_preserves_custody_through_actual_ingress
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Resume(None),
        GoalIngressEnding::BindCustodyDispose,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_terminal_predecessor_preserves_queued_start_custody() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_terminal_predecessor_preserves_queued_start_custody
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::TerminalCustodyDispose,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_final_reply_preserves_queued_resume_custody_for_disposal() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_final_reply_preserves_queued_resume_custody_for_disposal
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Resume(None),
        GoalIngressEnding::CustodyDispose,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_pause_is_queued_through_actual_ingress_until_parent_settlement() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_pause_is_queued_through_actual_ingress_until_parent_settlement
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::DeferredPause,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_budget_is_queued_and_enforced_after_parent_settlement() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_budget_is_queued_and_enforced_after_parent_settlement
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::DeferredBudget,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_immediate_pause_now_fences_before_parent_settlement() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_immediate_pause_now_fences_before_parent_settlement
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::ImmediatePause,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_immediate_stop_fences_before_parent_settlement() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_immediate_stop_fences_before_parent_settlement
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(GoalIngressAdmission::Start, GoalIngressEnding::Stop).await;
}

#[tokio::test]
async fn matrix_goal_pause_now_reports_drain_failure_after_acknowledgement() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_pause_now_reports_drain_failure_after_acknowledgement
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::ImmediatePauseDrainFailure,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_stop_reports_drain_failure_after_acknowledgement() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stop_reports_drain_failure_after_acknowledgement
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::StopDrainFailure,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_stop_admission_failure_keeps_error_detail_out_of_reply() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_stop_admission_failure_keeps_error_detail_out_of_reply
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::StopAdmissionFailure,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_deferred_settings_failure_reports_once_and_abandons_intent() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_deferred_settings_failure_reports_once_and_abandons_intent
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::SettingsFailure,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_deferred_claim_failure_reports_once_and_abandons_intent() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_deferred_claim_failure_reports_once_and_abandons_intent
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::ClaimFailure,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_deferred_audit_failure_reports_notice_without_command_error() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_deferred_audit_failure_reports_notice_without_command_error
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::AuditFailure,
    )
    .await;
}

#[tokio::test]
async fn matrix_goal_deferred_audit_failure_competing_start_waits_for_notice() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_deferred_audit_failure_competing_start_waits_for_notice
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_deferred_ingress(
        GoalIngressAdmission::Start,
        GoalIngressEnding::AuditFailureCompetingStart,
    )
    .await;
}

/// A real host owns one process-global ledger root. Isolate each install
/// fixture so unrelated tests cannot select that immutable root first.
async fn matrix_goal_ingress_delegated_to_fresh_process(test_name: &str) -> bool {
    const CHILD_CASE: &str = "ZEROCLAW_TEST_MATRIX_GOAL_INGRESS_CASE";
    let qualified = format!("orchestrator::tests::goal_deferred::{test_name}");
    if std::env::var(CHILD_CASE).as_deref() == Ok(qualified.as_str()) {
        return false;
    }
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", qualified.as_str(), "--test-threads=1"])
        .env(CHILD_CASE, &qualified)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .expect("isolated Matrix ingress test must finish within its process deadline")
        .expect("isolated Matrix ingress test must start");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated Matrix ingress test failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("test {qualified} ... ok")),
        "isolated Matrix ingress scenario did not execute\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    true
}

enum GoalIngressAdmission {
    Start,
    Resume(Option<&'static str>),
}

#[tokio::test]
async fn matrix_goal_pricing_preserves_ordinary_rates_after_reload() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_pricing_preserves_ordinary_rates_after_reload
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_constructor_pricing(true, true).await;
}

#[tokio::test]
async fn matrix_goal_pricing_works_without_ordinary_cost_tracking() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_pricing_works_without_ordinary_cost_tracking
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_constructor_pricing(false, true).await;
}

#[tokio::test]
async fn matrix_goal_unpriced_usage_preserves_normal_pause() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_unpriced_usage_preserves_normal_pause
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_constructor_pricing(true, false).await;
}

async fn assert_matrix_goal_constructor_pricing(ordinary_tracking: bool, priced: bool) {
    use zeroclaw_runtime::{
        agent::cost::build_type_level_model_provider_pricing,
        control_plane::{ControlPlaneHandle, TaskStatus},
        cost::CostTracker,
    };

    zeroclaw_log::try_install_capture_subscriber();
    let mut logs = zeroclaw_log::subscribe_or_install();
    let directory = tempfile::tempdir().unwrap();
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GoalIngressBoundaryProvider::default());
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.cost.enabled = ordinary_tracking;
    config.goal.verifier.model_provider = "custom.fixture".into();
    let profile = config.providers.models.ensure("custom", "fixture").unwrap();
    profile.live_pricing = false;
    profile.pricing.clear();
    if priced {
        profile.pricing.insert("test-model.input".into(), 1.0);
        profile.pricing.insert("test-model.output".into(), 2.0);
    }
    let ordinary_pricing = Arc::new(build_type_level_model_provider_pricing(&config));
    let expected_cost = if priced { 14.0 / 1_000_000.0 } else { 0.0 };
    let tracker =
        CostTracker::get_or_init_global_required(config.cost.clone(), directory.path()).unwrap();
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "custom.fixture",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    if ordinary_tracking {
        Arc::get_mut(&mut context).unwrap().cost_tracking = Some(ChannelCostTrackingState {
            tracker: tracker.clone(),
            model_provider_pricing: ordinary_pricing,
            agent_alias: context.agent_alias.clone(),
        });
    }
    let mut message = matrix_msg("@fixture:example.test");
    message.content = "ordinary pricing request".into();
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle);

    provider.ordinary_release.notify_one();
    tokio::time::timeout(
        Duration::from_secs(2),
        process_channel_message(context.clone(), message.clone(), CancellationToken::new()),
    )
    .await
    .expect("ordinary priced turn must finish");
    let ordinary = tracker.get_summary().unwrap();
    assert_eq!(ordinary.request_count, usize::from(ordinary_tracking));
    if ordinary_tracking {
        assert!((ordinary.session_cost_usd - expected_cost).abs() < 1e-12);
    }

    let snapshot = runtime_defaults_snapshot(&context);
    let mut reloaded = (*snapshot.config).clone();
    let profile = reloaded
        .providers
        .models
        .ensure("custom", "fixture")
        .unwrap();
    if priced {
        profile.pricing.insert("test-model.input".into(), 7.0);
        profile.pricing.insert("test-model.output".into(), 11.0);
    }
    *context.runtime_defaults_override.lock().unwrap() = Some(Arc::new(ChannelRuntimeOverride {
        config: Arc::new(reloaded),
        defaults: snapshot.defaults,
        generation: snapshot.generation + 1,
    }));
    // Reloaded turns resolve the generation-keyed ordinary provider cache.
    // Keep the scripted provider bound to that actual lookup boundary.
    context.provider_cache.lock().unwrap().insert(
        provider_cache_key("custom.fixture", None, snapshot.generation + 1),
        provider.clone(),
    );
    message.content = "/goal start --unlimited fixture pricing objective".into();
    route_matrix_goal_fixture_control(context.clone(), message.clone()).await;
    let replies: Vec<String> = channel
        .sent_messages
        .lock()
        .await
        .iter()
        .map(|reply| reply.content.clone())
        .collect();
    let mut admission_errors = Vec::new();
    while let Ok(frame) = logs.try_recv() {
        if frame["message"] == "Goal command submission failed" {
            admission_errors.push(frame["attributes"]["error"].clone());
        }
    }
    assert!(
        replies.iter().any(|reply| reply.contains("Goal started.")),
        "pricing fixture did not start its Goal: {replies:?}; errors={admission_errors:?}"
    );
    let outcome = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while provider.requests.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!("fresh Goal supervisor must admit its parent turn: {error}; replies={replies:?}")
        });
        let current = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.status, TaskStatus::Running);
        message.content = if priced {
            "/goal cancel"
        } else {
            "/goal pause"
        }
        .into();
        let mut control =
            zeroclaw_spawn::spawn!(route_matrix_goal_fixture_control(context.clone(), message,));
        tokio::time::timeout(Duration::from_secs(2), async {
            if priced {
                while registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
                    .status
                    != TaskStatus::Cancelled
                {
                    tokio::task::yield_now().await;
                }
            } else {
                let notice = channel_runtime_cli_string("goal-mode-queued-pause");
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content.contains(&notice))
                {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .unwrap()
                        .status,
                    TaskStatus::Running
                );
            }
        })
        .await
        .expect("control must be admitted before the held parent finishes");
        provider.goal_release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), &mut control)
            .await
            .expect("control must drain the released parent")
            .unwrap();
        let (tokens, cost, pricing_complete) = tracker
            .get_usage_totals_for_task_with_pricing(&current.id)
            .unwrap();
        assert_eq!(tokens, 12);
        assert_eq!(pricing_complete, priced);
        assert!(
            (cost - expected_cost).abs() < 1e-12,
            "Goal cost {cost} differs from ordinary cost {expected_cost}"
        );
        let goal = registry.get_goal_task(&current.id).await.unwrap().unwrap();
        assert_eq!(
            goal.accounting_state,
            zeroclaw_goal::GoalAccountingState::Complete
        );
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert!(goal.effective_cost_limit_usd.is_none());
        let final_task = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            final_task.status,
            if priced {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Paused
            }
        );
        if !priced {
            assert_eq!(
                goal.pause_reason,
                Some(zeroclaw_goal::GoalPauseReason::OperatorPaused)
            );
            let records = tokio::fs::read_to_string(directory.path().join("state/costs.jsonl"))
                .await
                .unwrap();
            let goal_records: Vec<zeroclaw_config::cost::types::CostRecord> = records
                .lines()
                .map(|line| {
                    serde_json::from_str::<zeroclaw_config::cost::types::CostRecord>(line).unwrap()
                })
                .filter(|record| record.task_id.as_deref() == Some(current.id.as_str()))
                .collect();
            assert_eq!(goal_records.len(), 1);
            assert_eq!(goal_records[0].usage.unpriced_tokens, 12);
            assert!(!goal_records[0].usage.pricing_available);
        }
        let total = tracker.get_summary().unwrap();
        assert_eq!(total.request_count, usize::from(ordinary_tracking) + 1);
        assert!((total.session_cost_usd - ordinary.session_cost_usd - cost).abs() < 1e-12);
        assert_eq!(provider.requests.lock().unwrap().len(), 2);
    };
    let result = std::panic::AssertUnwindSafe(outcome).catch_unwind().await;
    provider.goal_release.notify_one();
    provider.resumed_release.notify_one();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

enum GoalIngressEnding {
    Cancel,
    DeferredPause,
    DeferredBudget,
    ImmediatePause,
    Stop,
    ImmediatePauseDrainFailure,
    StopDrainFailure,
    StopAdmissionFailure,
    SettingsFailure,
    ClaimFailure,
    AuditFailure,
    AuditFailureCompetingStart,
    CustodyDispose,
    BindCustodyDispose,
    TerminalCustodyDispose,
    IdleRetire,
}

#[tokio::test]
async fn matrix_goal_new_refuses_only_normal_controls_until_reset_settles() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_refuses_only_normal_controls_until_reset_settles
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::NormalControls).await;
}

#[tokio::test]
async fn matrix_goal_new_closes_queued_budget_at_admission_before_lane_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_closes_queued_budget_at_admission_before_lane_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::QueuedBudget).await;
}

#[tokio::test]
async fn matrix_goal_new_settles_unbound_start_before_closed_reply() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_settles_unbound_start_before_closed_reply
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::QueuedStart).await;
}

#[tokio::test]
async fn matrix_goal_new_disposal_failure_preserves_history_and_releases_reservation() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_disposal_failure_preserves_history_and_releases_reservation
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::DisposalFailure).await;
}

#[tokio::test]
async fn matrix_goal_new_dropped_completion_preserves_history_and_releases_reservation() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_dropped_completion_preserves_history_and_releases_reservation
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::DroppedCompletion).await;
}

#[tokio::test]
async fn matrix_goal_new_cancelled_admission_releases_reservation_without_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_cancelled_admission_releases_reservation_without_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::CancelledAdmission).await;
}

#[tokio::test]
async fn matrix_goal_new_dropped_admission_releases_reservation_without_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_dropped_admission_releases_reservation_without_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::DroppedAdmission).await;
}

#[tokio::test]
async fn matrix_goal_new_full_lane_refusal_releases_reservation_without_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_full_lane_refusal_releases_reservation_without_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::RefusedAdmission).await;
}

#[tokio::test]
async fn matrix_goal_new_keeps_rejection_until_last_overlapping_reset_finishes() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_keeps_rejection_until_last_overlapping_reset_finishes
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::Overlapping).await;
}

enum MatrixGoalResetCase {
    NormalControls,
    RunningGoal,
    Passive,
    SelfAuthored,
    LaneOrder,
    QueuedBudget,
    QueuedStart,
    DisposalFailure,
    DroppedCompletion,
    CancelledAdmission,
    DroppedAdmission,
    RefusedAdmission,
    Overlapping,
    ImmediatePause,
    ImmediateCancel,
    ReadOnlyAndStop,
    Direct,
}

#[tokio::test]
async fn matrix_goal_new_disposes_running_goal_before_earlier_ordinary_turn() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_disposes_running_goal_before_earlier_ordinary_turn
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::RunningGoal).await;
}

#[tokio::test]
async fn matrix_goal_new_passive_message_preserves_running_goal() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_passive_message_preserves_running_goal
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::Passive).await;
}

#[tokio::test]
async fn matrix_goal_new_self_authored_message_preserves_running_goal() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_self_authored_message_preserves_running_goal
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::SelfAuthored).await;
}

#[tokio::test]
async fn matrix_goal_new_keeps_following_ordinary_turn_after_history_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_keeps_following_ordinary_turn_after_history_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::LaneOrder).await;
}

#[tokio::test]
async fn matrix_goal_new_immediate_pause_observes_disposal_before_lane_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_immediate_pause_observes_disposal_before_lane_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::ImmediatePause).await;
}

#[tokio::test]
async fn matrix_goal_new_immediate_cancel_observes_disposal_before_lane_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_immediate_cancel_observes_disposal_before_lane_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::ImmediateCancel).await;
}

#[tokio::test]
async fn matrix_goal_new_preserves_read_only_and_stop_controls() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_preserves_read_only_and_stop_controls
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::ReadOnlyAndStop).await;
}

#[tokio::test]
async fn matrix_goal_new_direct_path_holds_command_lease_through_reset() {
    if matrix_goal_ingress_delegated_to_fresh_process(stringify!(
        matrix_goal_new_direct_path_holds_command_lease_through_reset
    ))
    .await
    {
        return;
    }
    assert_matrix_goal_reset_ingress(MatrixGoalResetCase::Direct).await;
}

#[derive(Default)]
struct MatrixResetSendBarrier {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct HeldMatrixResetChannel {
    inner: Arc<SendMessageRecordingChannel>,
    reset_count: std::sync::atomic::AtomicUsize,
    barriers: [MatrixResetSendBarrier; 2],
}

impl zeroclaw_api::attribution::Attributable for HeldMatrixResetChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Matrix)
    }

    fn alias(&self) -> &str {
        "held-matrix-reset-fixture"
    }
}

#[async_trait::async_trait]
impl Channel for HeldMatrixResetChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    fn self_handle(&self) -> Option<String> {
        Some("@fixture-bot:example.test".into())
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        if message.content == channel_runtime_cli_string("channel-runtime-new-session") {
            let ordinal = self
                .reset_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(barrier) = self.barriers.get(ordinal) {
                barrier.entered.notify_one();
                barrier.release.notified().await;
            }
        }
        self.inner.send(message).await
    }

    async fn listen(&self, _: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn assert_matrix_goal_reset_ingress(case: MatrixGoalResetCase) {
    use zeroclaw_runtime::control_plane::{ControlPlaneHandle, TaskStatus};

    let queued_budget = matches!(case, MatrixGoalResetCase::QueuedBudget);
    let queued_start = matches!(case, MatrixGoalResetCase::QueuedStart);
    let disposal_failure = matches!(case, MatrixGoalResetCase::DisposalFailure);
    let dropped_completion = matches!(case, MatrixGoalResetCase::DroppedCompletion);
    let failed_reset = disposal_failure || dropped_completion;
    let cancelled_admission = matches!(case, MatrixGoalResetCase::CancelledAdmission);
    let dropped_admission = matches!(case, MatrixGoalResetCase::DroppedAdmission);
    let refused_admission = matches!(case, MatrixGoalResetCase::RefusedAdmission);
    let abandoned_admission = cancelled_admission || dropped_admission || refused_admission;
    let directory = tempfile::tempdir().unwrap();
    let verifier_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .mount(&verifier_server)
        .await;
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let provider = Arc::new(GoalIngressBoundaryProvider::default());
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let held_reset = matches!(
        case,
        MatrixGoalResetCase::Overlapping
            | MatrixGoalResetCase::Direct
            | MatrixGoalResetCase::SelfAuthored
    )
    .then(|| {
        Arc::new(HeldMatrixResetChannel {
            inner: channel.clone(),
            reset_count: std::sync::atomic::AtomicUsize::new(0),
            barriers: std::array::from_fn(|_| MatrixResetSendBarrier::default()),
        })
    });
    let presentation: Arc<dyn Channel> = match held_reset.as_ref() {
        Some(held) => held.clone(),
        None => channel.clone(),
    };
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider = "openai.fixture".into();
    let verifier = config.providers.models.ensure("openai", "fixture").unwrap();
    verifier.api_key = Some("synthetic-key".into());
    verifier.uri = Some(verifier_server.uri());
    verifier.kind = Some("openai-compatible".into());
    verifier.wire_api = Some(zeroclaw_config::schema::WireApi::ChatCompletions);
    verifier.model = Some("fixture-verifier".into());
    verifier.live_pricing = false;
    config
        .channels
        .matrix
        .get_mut("default")
        .unwrap()
        .interrupt_on_new_message = false;
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        presentation,
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    ));
    let mut message = matrix_msg("@fixture:example.test");
    message.content = "ordinary request before reset".into();
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle);
    if dropped_completion {
        foreground::drop_next_goal_test_reset_completion(&context.persist_locks, &key);
    }
    if cancelled_admission {
        foreground::cancel_next_goal_test_reset_admission(&context.persist_locks, &key);
    }
    if dropped_admission {
        foreground::drop_next_goal_test_reset_admission(&context.persist_locks, &key);
    }
    let audit = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
    if matches!(
        case,
        MatrixGoalResetCase::QueuedBudget
            | MatrixGoalResetCase::DisposalFailure
            | MatrixGoalResetCase::ImmediatePause
            | MatrixGoalResetCase::ImmediateCancel
            | MatrixGoalResetCase::ReadOnlyAndStop
            | MatrixGoalResetCase::Direct
    ) {
        let start = goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message.clone(),
            zeroclaw_commands::goal::GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Unlimited,
                objective: "fixture existing reset objective".into(),
            },
        )
        .await
        .unwrap();
        start.response_delivery_failed().await.unwrap();
        assert_eq!(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Paused
        );
        // Disposal may delete a bound intent before its waiter settles it.
        // Observe claims and applications without requiring a commit order.
        if queued_budget {
            audit
                .execute_batch(
                    "CREATE TABLE fixture_intent_settlements (state TEXT NOT NULL);
             CREATE TRIGGER fixture_record_intent_settlement
             AFTER UPDATE OF state ON goal_control_intents
             BEGIN INSERT INTO fixture_intent_settlements(state) VALUES (NEW.state); END;",
                )
                .unwrap();
        }
    }
    if disposal_failure {
        audit
            .execute_batch(
                "CREATE TRIGGER fixture_refuse_reset_disposal
             BEFORE UPDATE OF status ON tasks
             WHEN NEW.kind = 'goal' AND NEW.status = 'cancelled'
             BEGIN SELECT RAISE(ABORT, 'fixture reset disposal unavailable'); END;",
            )
            .unwrap();
    }
    if matches!(case, MatrixGoalResetCase::Direct) {
        assert_matrix_goal_direct_reset(
            context,
            provider,
            held_reset.unwrap(),
            registry,
            key,
            message,
        )
        .await;
        return;
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    let router = AgentRouter::single(context.clone());
    let mut dispatch = zeroclaw_spawn::spawn!(run_message_dispatch_loop(receiver, router, 4,));
    let outcome = async {
        if matches!(
            case,
            MatrixGoalResetCase::RunningGoal
                | MatrixGoalResetCase::Passive
                | MatrixGoalResetCase::SelfAuthored
        ) {
            message.content = "/goal start --unlimited fixture running reset objective".into();
        }
        sender.send(message.clone()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while provider.requests.lock().unwrap().len() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ordinary lane turn must be held before reset admission");
        if matches!(
            case,
            MatrixGoalResetCase::RunningGoal
                | MatrixGoalResetCase::Passive
                | MatrixGoalResetCase::SelfAuthored
        ) {
            assert_eq!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                TaskStatus::Running
            );
            if matches!(case, MatrixGoalResetCase::RunningGoal) {
                message.id = "fixture-earlier-ordinary".into();
                message.content = "ordinary request queued before reset".into();
                sender.send(message.clone()).await.unwrap();
            }
            message.id = "fixture-running-reset".into();
            message.content = "/new".into();
            message.passive_context = matches!(case, MatrixGoalResetCase::Passive);
            if matches!(case, MatrixGoalResetCase::SelfAuthored) {
                message.sender = "@fixture-bot:example.test".into();
            }
            sender.send(message.clone()).await.unwrap();
            if matches!(case, MatrixGoalResetCase::RunningGoal) {
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let current = registry.current_goal_for_session(&key).await.unwrap();
                        let current = current.expect("held Goal must remain present until drain");
                        if current.status == TaskStatus::Cancelled {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("reset admission must cancel the Goal while its model call is held");
                assert_eq!(provider.requests.lock().unwrap().len(), 1);
                assert!(
                    verifier_server
                        .received_requests()
                        .await
                        .unwrap()
                        .is_empty()
                );
                provider.ordinary_release.notify_one();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while provider.requests.lock().unwrap().len() < 2 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("earlier ordinary turn must run after the Goal worker releases its lease");
                assert!(provider.requests.lock().unwrap()[1].iter().any(|row| {
                    row.role == "user"
                        && row
                            .content
                            .ends_with("ordinary request queued before reset")
                }));
                // BR69(j): deletion and earlier ordinary admission are unordered after drain.
                tokio::time::timeout(Duration::from_secs(2), async {
                    while registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .is_some()
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("drained Goal must be deleted while the earlier ordinary call is held");
                assert!(!channel.sent_messages.lock().await.iter().any(|reply| {
                    reply.content == channel_runtime_cli_string("channel-runtime-new-session")
                }));
                provider.goal_release.notify_one();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !channel.sent_messages.lock().await.iter().any(|reply| {
                        reply.content == channel_runtime_cli_string("channel-runtime-new-session")
                    }) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("reset reply must follow Goal deletion and the earlier ordinary turn");
                assert!(
                    registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(provider.requests.lock().unwrap().len(), 2);
                assert!(
                    verifier_server
                        .received_requests()
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert!(!channel.sent_messages.lock().await.iter().any(|reply| {
                    reply.content == channel_runtime_cli_string("goal-mode-command-failed")
                }));
                return;
            }
            message.sender = "@fixture:example.test".into();
            message.passive_context = false;
            message.id = "fixture-preserved-budget".into();
            message.content = "/goal budget set --tokens 100".into();
            sender.send(message.clone()).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel.sent_messages.lock().await.iter().any(|reply| {
                    reply.content == channel_runtime_cli_string("goal-mode-queued-budget")
                }) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("inert reset must preserve normal budget admission");
            assert_eq!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                TaskStatus::Running
            );
            assert!(!channel.sent_messages.lock().await.iter().any(|reply| {
                reply.content == channel_runtime_cli_string("goal-mode-command-failed")
                    || reply.content == channel_runtime_cli_string("channel-runtime-new-session")
            }));
            message.id = "fixture-preservation-cancel".into();
            message.content = "/goal cancel".into();
            sender.send(message.clone()).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
                    .status
                    != TaskStatus::Cancelled
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("preservation cleanup must fence the running Goal");
            return;
        }
        if queued_budget || queued_start {
            message.id = "fixture-queued-budget".into();
            message.content = if queued_start {
                "/goal start --unlimited fixture queued reset objective"
            } else {
                "/goal budget set --tokens 100"
            }
            .into();
            sender.send(message.clone()).await.unwrap();
            let ack = channel_runtime_cli_string(if queued_start {
                "goal-mode-queued-start"
            } else {
                "goal-mode-queued-budget"
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == ack)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("control must be acknowledged before reset admission");
            assert_eq!(
                audit
                    .query_row(
                        "SELECT state FROM goal_control_intents WHERE session_id = ?1",
                        [&key],
                        |row| row.get::<_, String>(0)
                    )
                    .unwrap(),
                "pending"
            );
            if queued_start {
                assert!(
                    audit
                        .query_row(
                            "SELECT task_id FROM goal_control_intents WHERE session_id = ?1",
                            [&key],
                            |row| row.get::<_, Option<String>>(0)
                        )
                        .unwrap()
                        .is_none()
                );
            }
        }
        if refused_admission {
            // The real bounded queue is full; passive turns add no model work.
            for index in 0..CONVERSATION_LANE_BACKLOG_LIMIT {
                message.id = format!("fixture-passive-backlog-{index}");
                message.content = "fixture passive backlog context".into();
                message.passive_context = true;
                sender.send(message.clone()).await.unwrap();
            }
            message.passive_context = false;
        }
        message.id = "fixture-reset".into();
        message.content = "/new".into();
        sender.send(message.clone()).await.unwrap();
        if matches!(case, MatrixGoalResetCase::LaneOrder) {
            message.id = "fixture-following-ordinary".into();
            message.content = "ordinary request after reset".into();
            sender.send(message.clone()).await.unwrap();
            provider.ordinary_release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                while provider.requests.lock().unwrap().len() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("following ordinary turn must run after reset");
            assert!(!provider.requests.lock().unwrap()[1].iter().any(|row| {
                row.content.contains("ordinary boundary candidate")
                    || row.content == "ordinary request before reset"
            }));
            assert!(channel.sent_messages.lock().await.iter().any(|reply| {
                reply.content == channel_runtime_cli_string("channel-runtime-new-session")
            }));
            return;
        }
        if matches!(
            case,
            MatrixGoalResetCase::ImmediatePause | MatrixGoalResetCase::ImmediateCancel
        ) {
            message.id = "fixture-immediate-after-reset".into();
            message.content = if matches!(case, MatrixGoalResetCase::ImmediatePause) {
                "/goal pause now"
            } else {
                "/goal cancel"
            }
            .into();
            sender.send(message.clone()).await.unwrap();
            let no_current = channel_runtime_cli_string("goal-mode-no-current");
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == no_current)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("immediate control after new admission must see no Goal before lane reset");
            assert!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            assert!(!channel.sent_messages.lock().await.iter().any(
                |reply| reply.content == channel_runtime_cli_string("goal-mode-command-failed")
            ));
            provider.ordinary_release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel.sent_messages.lock().await.iter().any(|reply| {
                    reply.content == channel_runtime_cli_string("channel-runtime-new-session")
                }) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("reset must retain lane ordering after immediate query");
            return;
        }
        if matches!(case, MatrixGoalResetCase::ReadOnlyAndStop) {
            for (index, control) in ["/goal status", "/goal budget", "/goal help"]
                .iter()
                .enumerate()
            {
                message.id = format!("fixture-read-only-{index}");
                message.content = (*control).into();
                sender.send(message.clone()).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                while channel.sent_messages.lock().await.len() < 3 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("read-only controls must answer while reset is queued");
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            message.id = "fixture-stop-during-reset".into();
            message.content = "/stop".into();
            sender.send(message.clone()).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while channel.sent_messages.lock().await.len() < 4 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("ordinary stop must remain available during reset");
            assert!(!channel.sent_messages.lock().await.iter().any(
                |reply| reply.content == channel_runtime_cli_string("goal-mode-command-failed")
            ));
            assert_eq!(
                audit
                    .query_row(
                        "SELECT COUNT(*) FROM goal_control_intents WHERE session_id = ?1",
                        [&key],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            return;
        }
        if let Some(held) = held_reset.as_ref() {
            message.id = "fixture-second-reset".into();
            sender.send(message.clone()).await.unwrap();
            // An ordered read-only reply proves both resets passed admission
            // before releasing the ordinary lane turn.
            message.id = "fixture-reset-status".into();
            message.content = "/goal status".into();
            sender.send(message.clone()).await.unwrap();
            let no_current = channel_runtime_cli_string("goal-mode-no-current");
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == no_current)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("read-only status must follow both reset admissions");
            provider.ordinary_release.notify_one();
            let generic = channel_runtime_cli_string("goal-mode-command-failed");
            for (index, barrier) in held.barriers.iter().enumerate() {
                tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified())
                    .await
                    .expect("each native reset delivery must reach its held boundary");
                message.id = format!("fixture-overlap-start-{index}");
                message.content = "/goal start --unlimited fixture overlap objective".into();
                sender.send(message.clone()).await.unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while channel
                        .sent_messages
                        .lock()
                        .await
                        .iter()
                        .filter(|reply| reply.content == generic)
                        .count()
                        != index + 1
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("Start must stay refused until the last reset delivery settles");
                assert!(
                    registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(provider.requests.lock().unwrap().len(), 1);
                barrier.release.notify_one();
            }
            let reset_notice = channel_runtime_cli_string("channel-runtime-new-session");
            tokio::time::timeout(Duration::from_secs(2), async {
                while channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .filter(|reply| reply.content == reset_notice)
                    .count()
                    != 2
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("both ordered reset replies must finish before fresh Start");
        }
        if failed_reset {
            // Admission disposal fails while the ordinary turn stays held.
            // The reset reply and reservation release still belong to its lane.
            message.id = "fixture-reset-failure-refused-start".into();
            message.content = "/goal start --unlimited fixture refused reset objective".into();
            sender.send(message.clone()).await.unwrap();
            let failed = channel_runtime_cli_string("goal-mode-command-failed");
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == failed)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("normal Start must be refused while the failed reset waits in its lane");
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            let current = registry.current_goal_for_session(&key).await.unwrap();
            if disposal_failure {
                assert_eq!(current.unwrap().status, TaskStatus::Paused);
            } else {
                assert!(current.is_none());
            }
            provider.ordinary_release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                while channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .filter(|reply| reply.content == failed)
                    .count()
                    != 2
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("failed disposal must report the failed reset without hanging");
            assert!(!channel.sent_messages.lock().await.iter().any(|reply| {
                reply.content == channel_runtime_cli_string("channel-runtime-new-session")
            }));
            assert_eq!(
                audit
                    .query_row(
                        "SELECT COUNT(*) FROM goal_control_intents WHERE session_id = ?1",
                        [&key],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            if disposal_failure {
                audit
                    .execute_batch("DROP TRIGGER fixture_refuse_reset_disposal;")
                    .unwrap();
                message.id = "fixture-reset-failure-cancel-predecessor".into();
                message.content = "/goal cancel".into();
                sender.send(message.clone()).await.unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .unwrap()
                        .status
                        != TaskStatus::Cancelled
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("existing paused Goal must remain controllable after disposal failure");
            }
        }
        if queued_start {
            let no_current = channel_runtime_cli_string("goal-mode-no-current");
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == no_current)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("unbound Start must close while the ordinary turn remains held");
            assert_eq!(
                audit
                    .query_row(
                        "SELECT state, outcome_reason FROM goal_control_intents WHERE session_id = ?1",
                        [&key],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    )
                    .unwrap(),
                ("abandoned".into(), "session_closed".into())
            );
            assert!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(!channel.sent_messages.lock().await.iter().any(|reply| {
                reply.content == channel_runtime_cli_string("channel-runtime-new-session")
            }));
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            provider.ordinary_release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel.sent_messages.lock().await.iter().any(|reply| {
                    reply.content == channel_runtime_cli_string("channel-runtime-new-session")
                }) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("reset must follow the held ordinary turn");
            assert!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                audit.query_row(
                    "SELECT COUNT(*) FROM goal_control_intents WHERE session_id = ?1 AND state IN ('pending', 'claimed', 'applied')",
                    [&key],
                    |row| row.get::<_, i64>(0)
                ).unwrap(),
                0
            );
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            return;
        }
        if queued_budget {
            tokio::time::timeout(Duration::from_secs(2), async {
                while registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_some()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("new must dispose the Goal at admission while the lane turn remains held");
            tokio::time::timeout(Duration::from_secs(2), async {
                while channel.sent_messages.lock().await.len() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the disposed queued budget must receive its closed response before reset");
            assert!(
                audit
                    .query_row(
                        "SELECT COUNT(*) FROM fixture_intent_settlements WHERE state = 'abandoned'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap()
                    <= 1
            );
            assert_eq!(
                audit.query_row(
                    "SELECT COUNT(*) FROM fixture_intent_settlements WHERE state IN ('claimed', 'superseded')",
                    [],
                    |row| row.get::<_, i64>(0)
                ).unwrap(),
                0
            );
            assert_eq!(
                audit.query_row(
                    "SELECT COUNT(*) FROM goal_control_intents WHERE session_id = ?1 AND (kind = 'set_budget' OR state IN ('pending', 'claimed'))",
                    [&key],
                    |row| row.get::<_, i64>(0)
                ).unwrap(),
                0
            );
            assert_eq!(
                audit
                    .query_row(
                        "SELECT COUNT(*) FROM fixture_intent_settlements WHERE state = 'applied'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            let replies = channel.sent_messages.lock().await;
            assert_eq!(replies.len(), 2);
            assert!(
                [
                    "goal-mode-no-current",
                    "goal-mode-already-paused",
                    "goal-mode-terminal",
                    "goal-mode-stale"
                ]
                .iter()
                .any(|key| replies[1]
                    .content
                    .starts_with(&channel_runtime_cli_string(key)))
            );
            assert!(
                !replies.iter().any(|reply| reply.content
                    == channel_runtime_cli_string("channel-runtime-new-session"))
            );
            drop(replies);
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            provider.ordinary_release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel.sent_messages.lock().await.iter().any(|reply| {
                    reply.content == channel_runtime_cli_string("channel-runtime-new-session")
                }) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("history reset must finish after the lane turn, not at admission");
            assert_eq!(
                audit
                    .query_row(
                        "SELECT COUNT(*) FROM fixture_intent_settlements WHERE state = 'applied'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            assert!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_none()
            );
            return;
        }
        if abandoned_admission {
            if refused_admission {
                let busy = channel_runtime_cli_string("channel-runtime-conversation-busy");
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !channel
                        .sent_messages
                        .lock()
                        .await
                        .iter()
                        .any(|reply| reply.content == busy)
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("full lane must refuse the reset at actual admission");
            }
            message.id = "fixture-start-after-cancelled-reset".into();
            message.content = "/goal start --unlimited fixture retained history objective".into();
            sender.send(message.clone()).await.unwrap();
            let queued = channel_runtime_cli_string("goal-mode-queued-start");
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == queued)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("abandoned reset must release its reservation before later Start admission");
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            assert!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(!channel.sent_messages.lock().await.iter().any(|reply| {
                reply.content == channel_runtime_cli_string("channel-runtime-new-session")
                    || reply.content == channel_runtime_cli_string("goal-mode-command-failed")
            }));
            provider.ordinary_release.notify_one();
        }
        if held_reset.is_none() && !failed_reset && !abandoned_admission {
            let controls = [
                "/goal start --unlimited fixture reset objective",
                "/goal resume",
                "/goal resume fixture answer\nsecond line",
                "/goal pause",
                "/goal budget set --tokens 100",
            ];
            for (index, control) in controls.iter().enumerate() {
                message.id = format!("fixture-refused-{index}");
                message.content = (*control).into();
                sender.send(message.clone()).await.unwrap();
            }
            let generic = channel_runtime_cli_string("goal-mode-command-failed");
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let replies = channel.sent_messages.lock().await;
                    if replies
                        .iter()
                        .filter(|reply| reply.content == generic)
                        .count()
                        == controls.len()
                    {
                        break;
                    }
                    drop(replies);
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("every normal control must be refused while the reset waits for its lane");
            assert!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                foreground::goal_supervisor_slot(&context.persist_locks, &key)
                    .lock()
                    .await
                    .is_none()
            );
            assert_eq!(
                audit
                    .query_row(
                        "SELECT COUNT(*) FROM goal_control_intents WHERE session_id = ?1",
                        [&key],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            assert_eq!(provider.requests.lock().unwrap().len(), 1);

            for (index, control) in [
                "/goal cancel",
                "/goal pause now",
                "/goal status",
                "/goal budget",
                "/goal help",
            ]
            .iter()
            .enumerate()
            {
                message.id = format!("fixture-allowed-{index}");
                message.content = (*control).into();
                sender.send(message.clone()).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                while channel.sent_messages.lock().await.len() < controls.len() + 5 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("immediate and read-only controls must remain available");
            assert_eq!(
                channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .filter(|reply| reply.content == generic)
                    .count(),
                controls.len()
            );
            provider.ordinary_release.notify_one();
            let reset_notice = channel_runtime_cli_string("channel-runtime-new-session");
            tokio::time::timeout(Duration::from_secs(2), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == reset_notice)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("reset must finish after the ordinary lane turn");
        }

        if !abandoned_admission {
            message.id = "fixture-fresh-start".into();
            message.content = "/goal start --unlimited fixture reset objective".into();
            sender.send(message.clone()).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while provider.requests.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a normal Start must be admitted after reset completion");
        assert_eq!(
            provider.requests.lock().unwrap()[1]
                .iter()
                .any(|row| row.content.contains("ordinary boundary candidate")),
            failed_reset || abandoned_admission,
            "only a successful reset may clear the prior ordinary history"
        );
        message.content = "/goal cancel".into();
        sender.send(message.clone()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap()
                .status
                != TaskStatus::Cancelled
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cleanup cancel must fence the fresh Goal");
    };
    let result = std::panic::AssertUnwindSafe(outcome).catch_unwind().await;
    provider.ordinary_release.notify_one();
    provider.goal_release.notify_one();
    provider.resumed_release.notify_one();
    if let Some(held) = held_reset.as_ref() {
        for barrier in &held.barriers {
            barrier.release.notify_one();
        }
    }
    drop(sender);
    let drained = tokio::time::timeout(Duration::from_secs(3), &mut dispatch).await;
    if drained.is_err() {
        dispatch.abort();
    }
    assert!(
        drained.is_ok(),
        "reset fixture ingress must drain after release"
    );
    drained.unwrap().unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert!(
        verifier_server
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
}

async fn assert_matrix_goal_direct_reset(
    context: Arc<ChannelRuntimeContext>,
    provider: Arc<GoalIngressBoundaryProvider>,
    channel: Arc<HeldMatrixResetChannel>,
    registry: Arc<dyn zeroclaw_goal::GoalTaskRegistry>,
    key: String,
    mut message: ChannelMessage,
) {
    message.content = "/new".into();
    let reset_context = context.clone();
    let reset_message = message.clone();
    let mut reset = zeroclaw_spawn::spawn!(process_channel_message(
        reset_context,
        reset_message,
        CancellationToken::new()
    ));
    let outcome = async {
        tokio::time::timeout(
            Duration::from_secs(2),
            channel.barriers[0].entered.notified(),
        )
        .await
        .expect("direct reset must dispose before native reset delivery");
        assert!(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            foreground::goal_command_lock(&context.persist_locks, &key)
                .try_lock_owned()
                .is_err(),
            "direct reset must retain command serialization through history reset delivery"
        );
        let barrier =
            foreground::install_goal_test_bind_barrier(&context.persist_locks, &key, false);
        let start_context = context.clone();
        let start_key = key.clone();
        let start_message = message.clone();
        let mut start = zeroclaw_spawn::spawn!(goal_execution::submit_matrix_goal(
            start_context,
            start_key,
            start_message,
            zeroclaw_commands::goal::GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Unlimited,
                objective: "fixture post-reset direct objective".into(),
            },
        ));
        channel.barriers[0].release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), &mut reset)
            .await
            .expect("direct reset must finish")
            .unwrap();
        let entered =
            tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified()).await;
        barrier.release.notify_one();
        entered.expect("waiting Start must bind after reset completion");
        let submission = tokio::time::timeout(Duration::from_secs(2), &mut start)
            .await
            .expect("fresh direct Start must be admitted")
            .unwrap()
            .unwrap();
        assert!(matches!(
            submission.response(),
            zeroclaw_goal::GoalResponse::Started(_)
        ));
        submission.response_delivery_failed().await.unwrap();
        assert!(provider.requests.lock().unwrap().is_empty());
    };
    let result = std::panic::AssertUnwindSafe(outcome).catch_unwind().await;
    for barrier in &channel.barriers {
        barrier.release.notify_one();
    }
    if !reset.is_finished() {
        let drained = tokio::time::timeout(Duration::from_secs(2), &mut reset).await;
        if drained.is_err() {
            reset.abort();
        }
        assert!(
            drained.is_ok(),
            "direct reset must drain after fixture release"
        );
        drained.unwrap().unwrap();
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Drive a control through ordered ingress and join its native delivery task.
/// Its isolated task tracker lets the fixture observe completed cleanup while
/// a different, ordinary conversation turn remains held.
async fn route_matrix_goal_fixture_control(
    context: Arc<ChannelRuntimeContext>,
    message: ChannelMessage,
) {
    let notices = IngressTaskTracker::new();
    let lanes = ConversationLaneRegistry::new(
        Arc::new(tokio::sync::Semaphore::new(4)),
        Default::default(),
        CancellationToken::new(),
    );
    let delivery_message_id = message.id.clone();
    let order = IngressOrderRegistry::new().register(&conversation_history_key(&message));
    let pending_work = Arc::new(tokio::sync::Semaphore::new(1))
        .acquire_owned()
        .await
        .unwrap();
    let slot = InboundSlot::Ready(InboundTurn {
        turn: Box::new(PendingTurn {
            matrix_goal_reset: None,
            ctx: context,
            agent_generation: 0,
            msg: message,
            dispatch_ownership: ModelPickerDispatchOwnership::hold(&delivery_message_id),
            delivery_message_id,
            registration: None,
            pending_work,
        }),
        order,
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        route_inbound_slot(
            lanes.clone(),
            Arc::new(tokio::sync::Semaphore::new(1)),
            notices.clone(),
            slot,
        )
        .await;
        notices.wait_drained().await;
        lanes.wait_drained().await;
    })
    .await
    .expect("immediate control ingress and final delivery must settle before the held turn");
}

/// Hold one failed-row read before native failure publication joins the FIFO.
#[derive(Default)]
struct MatrixAuditFailureGate {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
    claimed: std::sync::atomic::AtomicBool,
}

struct MatrixAuditFailureRegistry {
    registry: Arc<dyn zeroclaw_goal::GoalTaskRegistry>,
    gate: Arc<MatrixAuditFailureGate>,
}

// All methods still reach the concrete store; only one observational read waits.
macro_rules! forward_matrix_audit_registry {
    ($($name:ident($($argument:ident: $ty:ty),*) -> $result:ty;)*) => {
        #[async_trait::async_trait]
        impl zeroclaw_goal::GoalTaskRegistry for MatrixAuditFailureRegistry {
            $(async fn $name(&self, $($argument: $ty),*) -> $result {
                self.registry.$name($($argument),*).await
            })*

            async fn current_goal_for_session(
                &self, session_id: &str,
            ) -> anyhow::Result<Option<zeroclaw_api::task::TaskRecord>> {
                let task = self.registry.current_goal_for_session(session_id).await?;
                if task.as_ref().is_some_and(|task| task.status == zeroclaw_api::task::TaskStatus::Failed)
                    && !self.gate.claimed.swap(true, Ordering::AcqRel)
                {
                    self.gate.reached.notify_one();
                    self.gate.release.notified().await;
                }
                Ok(task)
            }
        }
    };
}

forward_matrix_audit_registry! {
    record_control_intent(intent: &zeroclaw_goal::task::GoalControlIntent) -> anyhow::Result<()>;
    claim_control_intent(intent_id: &str, owner_boot_id: &str) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    settle_control_intent(intent_id: &str, settlement: zeroclaw_goal::task::GoalControlIntentSettlement) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    latest_active_goal_for_agent(agent: &str) -> anyhow::Result<Option<zeroclaw_api::task::TaskRecord>>;
    latest_active_goal_for_context(agent: &str, route: Option<&str>, principal: Option<&str>) -> anyhow::Result<Option<zeroclaw_api::task::TaskRecord>>;
    latest_active_goal_id_for_context(agent: &str, route: Option<&str>, principal: Option<&str>) -> anyhow::Result<Option<String>>;
    get_goal_task(task_id: &str) -> anyhow::Result<Option<zeroclaw_goal::GoalTaskRecord>>;
    update_goal_limits(task_id: &str, tokens: Option<u64>, cost: Option<f64>) -> anyhow::Result<()>;
    update_goal_pause(task_id: &str, pause: Option<zeroclaw_goal::GoalPauseState>) -> anyhow::Result<()>;
    set_continuation_context(task_id: &str, context: Option<zeroclaw_goal::task::TaskContinuationContext>) -> anyhow::Result<()>;
    get_continuation_context(task_id: &str) -> anyhow::Result<Option<zeroclaw_goal::task::TaskContinuationContext>>;
    terminal_reason_for_session_goal(task_id: &str, session_id: &str) -> anyhow::Result<Option<String>>;
    create_or_replace_session_goal(task: zeroclaw_api::task::TaskRecord, goal: zeroclaw_goal::GoalTaskRecord) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    pause_session_goal(task_id: &str, session_id: &str, epoch: i64, pause: zeroclaw_goal::GoalPauseState) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    resume_session_goal(task_id: &str, session_id: &str, epoch: i64, owner_pid: u32, boot_id: &str) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    finish_session_goal(task_id: &str, session_id: &str, epoch: i64, status: zeroclaw_api::task::TaskStatus, error: Option<String>) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    admit_pending_operation(task_id: &str, session_id: &str, epoch: i64, call_id: &str) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    settle_pending_operation(task_id: &str, session_id: &str, epoch: i64, call_id: &str, state: zeroclaw_goal::GoalAccountingState) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    update_session_goal_limits(task_id: &str, session_id: &str, epoch: i64, tokens: Option<u64>, cost: Option<f64>) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
    delete_session_goal(task_id: &str, session_id: &str, epoch: i64) -> anyhow::Result<zeroclaw_goal::GoalTransitionResult>;
}

async fn assert_matrix_goal_deferred_ingress(
    admission: GoalIngressAdmission,
    ending: GoalIngressEnding,
) {
    use zeroclaw_goal::GoalRuntime;
    use zeroclaw_infra::session_backend::SessionBackend;
    use zeroclaw_runtime::{
        control_plane::{ControlPlaneHandle, TaskStatus},
        cost::CostTracker,
        goal_accounting::{ModelProviderGoalUsagePricer, RuntimeGoalOperationAccountingFactory},
    };

    let directory = tempfile::tempdir().unwrap();
    let verifier_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .expect(0)
        .mount(&verifier_server)
        .await;
    let handle = ControlPlaneHandle::open(directory.path()).unwrap();
    let registry = handle.goal_store().unwrap();
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GoalIngressBoundaryProvider::default());
    let mut config = matrix_control_test_config();
    config.data_dir = directory.path().to_owned();
    config.goal.enabled = true;
    config.goal.verifier.model_provider =
        zeroclaw_config::providers::ModelProviderRef::new("openai.fixture");
    let verifier = config.providers.models.ensure("openai", "fixture").unwrap();
    verifier.api_key = Some("synthetic-key".into());
    verifier.uri = Some(verifier_server.uri());
    verifier.kind = Some("openai-compatible".into());
    verifier.wire_api = Some(zeroclaw_config::schema::WireApi::ChatCompletions);
    verifier.model = Some("fixture-verifier".into());
    verifier.live_pricing = false;
    config.channels.debounce_ms = 1000;
    let live_config = zeroclaw_config::live::LiveConfig::new(config.clone());
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    Arc::get_mut(&mut context).unwrap().live_config = live_config.handle();
    let backend = Arc::new(
        zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(directory.path()).unwrap(),
    );
    Arc::get_mut(&mut context).unwrap().session_store = Some(backend.clone());
    let mut message = matrix_msg("@fixture:example.test");
    message.content = "ordinary boundary request".into();
    let key = runtime_conversation_history_key(&context, &message);
    foreground::install_goal_test_control_plane(&context.persist_locks, &key, handle.clone());
    let tracker = CostTracker::get_or_init_global_required(
        zeroclaw_config::schema::CostConfig::default(),
        directory.path(),
    )
    .unwrap();
    let accounting = Arc::new(
        RuntimeGoalOperationAccountingFactory::new(
            registry.clone(),
            tracker.clone(),
            context.agent_alias.to_string(),
            Arc::new(ModelProviderGoalUsagePricer::new(Arc::default())),
        )
        .unwrap(),
    );
    let audit_gate = matches!(ending, GoalIngressEnding::AuditFailureCompetingStart)
        .then(|| Arc::new(MatrixAuditFailureGate::default()));
    let execution_registry: Arc<dyn zeroclaw_goal::GoalTaskRegistry> =
        if let Some(gate) = audit_gate.as_ref() {
            Arc::new(MatrixAuditFailureRegistry {
                registry: registry.clone(),
                gate: gate.clone(),
            })
        } else {
            registry.clone()
        };
    let supervisor = handle
        .goal_execution_restart()
        .new_supervisor(Arc::new(
            GoalRuntime::new(execution_registry).execution_engine(accounting),
        ))
        .await;
    *foreground::goal_supervisor_slot(&context.persist_locks, &key)
        .lock()
        .await = Some(supervisor.clone());
    let resume = matches!(admission, GoalIngressAdmission::Resume(_));
    let terminal_predecessor = matches!(ending, GoalIngressEnding::TerminalCustodyDispose);
    let predecessor = if resume || terminal_predecessor {
        let initial = goal_execution::submit_matrix_goal(
            context.clone(),
            key.clone(),
            message.clone(),
            zeroclaw_commands::goal::GoalCommand::Start {
                budget: zeroclaw_commands::goal::GoalBudgetSelection::Unlimited,
                objective: "fixture boundary objective".into(),
            },
        )
        .await
        .unwrap();
        // Prepare a real paused Goal through the existing failed-delivery
        // settlement, without launching a synthetic predecessor worker.
        initial.response_delivery_failed().await.unwrap();
        assert!(
            foreground::goal_supervisor_slot(&context.persist_locks, &key)
                .lock()
                .await
                .is_none(),
            "failed delivery must release an idle supervisor slot"
        );
        assert!(supervisor.try_retire().await);
        if terminal_predecessor {
            let mut cancellation = goal_execution::submit_matrix_goal(
                context.clone(),
                key.clone(),
                message.clone(),
                zeroclaw_commands::goal::GoalCommand::Cancel,
            )
            .await
            .unwrap();
            assert!(matches!(
                cancellation.response(),
                zeroclaw_goal::GoalResponse::Cancelled(_)
            ));
            assert!(cancellation.after_response().await.unwrap().is_none());
            cancellation.final_response_delivered(None).await;
        }
        // Reinstall a retired owner to exercise cold admission's typed
        // reselection through real ingress rather than only an empty slot.
        *foreground::goal_supervisor_slot(&context.persist_locks, &key)
            .lock()
            .await = Some(supervisor.clone());
        backend
            .append(
                &key,
                &zeroclaw_api::model_provider::ChatMessage::user("fixture initial request"),
            )
            .unwrap();
        backend
            .append(
                &key,
                &zeroclaw_api::model_provider::ChatMessage::assistant(
                    "Please choose fixture option A or B.",
                ),
            )
            .unwrap();
        let task = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            task.status,
            if terminal_predecessor {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Paused
            }
        );
        assert_eq!(
            task.execution_epoch,
            if terminal_predecessor { 3 } else { 2 }
        );
        Some((task.id, task.execution_epoch))
    } else {
        None
    };
    let audit = rusqlite::Connection::open(directory.path().join("control_plane.db")).unwrap();
    let intent_state = || {
        audit
            .query_row(
                "SELECT state FROM goal_control_intents WHERE session_id = ?1",
                [&key],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
    };
    let unsettled_intents = || {
        audit
            .query_row(
                "SELECT COUNT(*) FROM goal_control_intents WHERE session_id = ?1 AND state IN ('pending', 'claimed')",
                [&key],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let bind_barrier = matches!(ending, GoalIngressEnding::BindCustodyDispose)
        .then(|| foreground::install_goal_test_bind_barrier(&context.persist_locks, &key, false));
    let router = AgentRouter::single(context.clone());
    let mut dispatch = zeroclaw_spawn::spawn!(run_message_dispatch_loop(receiver, router, 4,));
    sender.send(message.clone()).await.unwrap();
    let mut launched_scope = None;
    let outcome = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while provider.requests.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ordinary turn must enter before the Goal control");
        let held_history = backend.load(&key);
        if matches!(ending, GoalIngressEnding::IdleRetire) {
            message.content = "/goal pause now".into();
            route_matrix_goal_fixture_control(context.clone(), message.clone()).await;
            let replies = channel.sent_messages.lock().await;
            assert_eq!(replies.len(), 1, "idle control must produce one result");
            assert!(
                replies[0]
                    .content
                    .starts_with(&channel_runtime_cli_string("goal-mode-already-paused"))
            );
            drop(replies);
            assert!(
                foreground::goal_supervisor_slot(&context.persist_locks, &key)
                    .lock()
                    .await
                    .is_none(),
                "an idle final reply must retire its supervisor"
            );
            assert_eq!(unsettled_intents(), 0);
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            assert_eq!(
                serde_json::to_value(backend.load(&key)).unwrap(),
                serde_json::to_value(&held_history).unwrap()
            );
            return;
        }
        let response = match admission {
            GoalIngressAdmission::Start => None,
            GoalIngressAdmission::Resume(response) => response,
        };
        message.content = match admission {
            GoalIngressAdmission::Resume(Some(response)) => format!("/goal resume {response}"),
            GoalIngressAdmission::Resume(None) => "/goal resume".into(),
            GoalIngressAdmission::Start => {
                "/goal start --unlimited fixture boundary objective".into()
            }
        };
        sender.send(message.clone()).await.unwrap();
        if let Some(barrier) = bind_barrier.as_ref() {
            tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified())
                .await
                .expect("actual ingress must reach the admitted driver's bind");
            let slot = foreground::goal_supervisor_slot(&context.persist_locks, &key);
            let resident = slot.lock().await.as_ref().cloned().unwrap();
            assert!(
                !tokio::time::timeout(Duration::from_millis(250), resident.try_retire())
                    .await
                    .expect("retirement must not wait for an admission at bind"),
                "an admission at bind must retain supervisor custody"
            );
            assert!(
                slot.lock()
                    .await
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resident))
            );
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            barrier.release.notify_one();
        }
        let queued = channel_runtime_cli_string(if resume {
            "goal-mode-queued-resume"
        } else {
            "goal-mode-queued-start"
        });
        tokio::time::timeout(Duration::from_millis(250), async {
            while !channel
                .sent_messages
                .lock()
                .await
                .iter()
                .any(|reply| reply.content == queued)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("valid Goal control must acknowledge before the ordinary turn ends");
        let before = registry.current_goal_for_session(&key).await.unwrap();
        if let Some((id, epoch)) = predecessor.as_ref() {
            let task = before.unwrap();
            assert_eq!(&task.id, id);
            assert_eq!(
                task.status,
                if terminal_predecessor {
                    TaskStatus::Cancelled
                } else {
                    TaskStatus::Paused
                }
            );
            assert_eq!(task.execution_epoch, *epoch);
            if let Some(response) = response {
                assert!(backend.load(&key).iter().all(|row| row.content != response));
            }
            assert_eq!(
                serde_json::to_value(backend.load(&key)).unwrap(),
                serde_json::to_value(&held_history).unwrap(),
                "queued control must preserve the admitted ordinary history"
            );
        } else {
            assert!(before.is_none());
        }
        assert_eq!(intent_state(), "pending");
        let intent_count: i64 = audit
            .query_row(
                "SELECT COUNT(*) FROM goal_control_intents WHERE session_id = ?1",
                [&key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(intent_count, 1, "reselection must admit the control once");
        if resume {
            let slot = foreground::goal_supervisor_slot(&context.persist_locks, &key);
            let selected = slot.lock().await.as_ref().cloned().unwrap();
            assert!(
                !Arc::ptr_eq(&selected, &supervisor),
                "ingress must replace the retired supervisor before admission"
            );
        }
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        if matches!(ending, GoalIngressEnding::ClaimFailure) {
            audit
                .execute_batch(
                    "CREATE TRIGGER fixture_refuse_goal_claim
                     BEFORE UPDATE OF state ON goal_control_intents
                     WHEN NEW.state = 'claimed'
                     BEGIN SELECT RAISE(ABORT, 'private fixture claim detail'); END;",
                )
                .unwrap();
            return;
        }
        if matches!(
            ending,
            GoalIngressEnding::AuditFailure | GoalIngressEnding::AuditFailureCompetingStart
        ) {
            // Fail settlement after the real deferred transition, not admission.
            audit
                .execute_batch(
                    "CREATE TRIGGER fixture_refuse_goal_audit
                     BEFORE UPDATE OF state ON goal_control_intents
                     WHEN NEW.state = 'applied'
                     BEGIN SELECT RAISE(ABORT, 'private fixture audit detail'); END;",
                )
                .unwrap();
            return;
        }
        if matches!(ending, GoalIngressEnding::SettingsFailure) {
            // Corrupt only the fixture's live settings after valid admission.
            // The production delivery path must resolve them again at the
            // checkpoint, abandon its ready intent, and publish one failure.
            let mut config = live_config.snapshot();
            config.goal.default_cost_limit_usd = Some(f64::NAN);
            live_config
                .publish(live_config.next_revision().unwrap(), config)
                .unwrap();
            return;
        }
        if matches!(
            ending,
            GoalIngressEnding::CustodyDispose
                | GoalIngressEnding::BindCustodyDispose
                | GoalIngressEnding::TerminalCustodyDispose
        ) {
            // Join the second control's real ingress delivery so custody is
            // checked after cleanup, not in the race between send and cleanup.
            let slot = foreground::goal_supervisor_slot(&context.persist_locks, &key);
            let resident = slot.lock().await.as_ref().unwrap().clone();
            let mut pause_message = message.clone();
            pause_message.content = if terminal_predecessor {
                "/goal status"
            } else {
                "/goal pause now"
            }
            .into();
            route_matrix_goal_fixture_control(context.clone(), pause_message).await;
            assert!(
                slot.lock()
                    .await
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resident)),
                "terminal predecessor or final reply must retain queued-control custody"
            );
            let before_disposal = channel.sent_messages.lock().await.len();
            tokio::time::timeout(
                Duration::from_secs(2),
                goal_execution::dispose_matrix_goal(&context, &key),
            )
            .await
            .expect("disposal must close queued custody before releasing the ordinary turn")
            .unwrap();
            // The task FK deletes its intent after disposal. Observe the
            // closed native reply, not a row whose owner no longer exists.
            tokio::time::timeout(Duration::from_secs(2), async {
                while channel.sent_messages.lock().await.len() == before_disposal {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("closed control reply must arrive before the ordinary turn is released");
            let replies = channel.sent_messages.lock().await;
            assert_eq!(replies.len(), before_disposal + 1);
            let closed = &replies[before_disposal].content;
            assert!(
                [
                    "goal-mode-no-current",
                    "goal-mode-already-paused",
                    "goal-mode-terminal",
                    "goal-mode-stale"
                ]
                .iter()
                .any(|key| closed.starts_with(&channel_runtime_cli_string(key))),
                "disposal must publish a closed-control response, not a generic error: {closed}"
            );
            drop(replies);
            assert_eq!(unsettled_intents(), 0);
            assert!(
                registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(slot.lock().await.is_none());
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            return;
        }
        provider.ordinary_release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), async {
            while provider.requests.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("one Goal parent must begin after the ordinary turn");
        let task = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(
            task.execution_epoch,
            predecessor.as_ref().map_or(1, |(_, epoch)| epoch + 1)
        );
        launched_scope = Some(
            zeroclaw_goal::GoalExecutionScope::new(
                task.id.clone(),
                key.clone(),
                task.execution_epoch,
            )
            .unwrap(),
        );
        if let Some((id, _)) = predecessor.as_ref() {
            assert_eq!(&task.id, id);
            let requests = provider.requests.lock().unwrap();
            assert!(requests[1].iter().any(|row| row.role == "assistant"
                && row.content == "Please choose fixture option A or B."));
            let durable = backend.load(&key);
            if let Some(response) = response {
                assert_eq!(
                    requests[1]
                        .iter()
                        .filter(|row| row.role == "user" && row.content == response)
                        .count(),
                    1
                );
                assert_eq!(
                    durable
                        .iter()
                        .filter(|row| row.role == "user" && row.content == response)
                        .count(),
                    1
                );
            } else {
                let expected_user_messages: Vec<_> = held_history
                    .iter()
                    .filter(|row| row.role == "user")
                    .map(|row| row.content.as_str())
                    .collect();
                let user_messages: Vec<_> = durable
                    .iter()
                    .filter(|row| row.role == "user")
                    .map(|row| row.content.as_str())
                    .collect();
                assert_eq!(
                    user_messages, expected_user_messages,
                    "plain resume must not append a fabricated user response"
                );
                assert_eq!(expected_user_messages.len(), 2);
                assert_eq!(durable.len(), held_history.len() + 1);
                assert_eq!(
                    serde_json::to_value(&durable[..held_history.len()]).unwrap(),
                    serde_json::to_value(&held_history).unwrap(),
                    "plain resume must retain the original durable question and admitted input"
                );
            }
        }
        assert_eq!(intent_state(), "applied");
        let replies = channel.sent_messages.lock().await;
        let ordinary = replies
            .iter()
            .position(|reply| reply.content == "ordinary boundary candidate")
            .unwrap();
        let lifecycle = replies
            .iter()
            .position(|reply| {
                reply.content.contains(if resume {
                    "Goal resumed."
                } else {
                    "Goal started."
                })
            })
            .unwrap();
        assert_eq!(
            lifecycle,
            ordinary + 1,
            "started precedes every Goal execution event"
        );
        drop(replies);
        if matches!(ending, GoalIngressEnding::StopAdmissionFailure) {
            zeroclaw_log::try_install_capture_subscriber();
            drop(zeroclaw_log::subscribe_or_install());
            let mut logs = zeroclaw_log::subscribe().unwrap();
            let token = ["sk", "_live_", "0123456789abcdefghijklmnopqrstuv"].concat();
            audit
                .execute_batch(&format!(
                    "CREATE TRIGGER fixture_refuse_goal_pause
                     BEFORE UPDATE OF status ON tasks
                     WHEN NEW.kind = 'goal' AND NEW.status = 'paused'
                     BEGIN SELECT RAISE(ABORT, 'private fixture pause detail {token}'); END;"
                ))
                .unwrap();
            message.content = "/stop".into();
            sender.send(message.clone()).await.unwrap();
            let failed = channel_runtime_cli_string("goal-mode-command-failed");
            let reply = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Some(reply) = channel
                        .sent_messages
                        .lock()
                        .await
                        .iter()
                        .find(|reply| reply.content.contains(&failed))
                        .map(|reply| reply.content.clone())
                    {
                        break reply;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("a pause admission failure must retain the ordinary stop reply");
            let unchanged = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            // Remove the fault and fence the real worker before surfacing
            // assertions, so even a red egress check cannot admit a verifier.
            audit
                .execute_batch("DROP TRIGGER fixture_refuse_goal_pause")
                .unwrap();
            message.content = "/goal cancel".into();
            sender.send(message.clone()).await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
                    .status
                    != TaskStatus::Cancelled
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("fixture cleanup must fence the held worker before release");
            assert_eq!(unchanged.id, task.id);
            assert_eq!(unchanged.status, TaskStatus::Running);
            assert_eq!(unchanged.execution_epoch, task.execution_epoch);
            assert_eq!(provider.requests.lock().unwrap().len(), 2);
            let frame = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let frame = logs.recv().await.unwrap();
                    if frame["message"] == "Goal /stop pause could not be admitted" {
                        break frame;
                    }
                }
            })
            .await
            .expect("pause admission failure must retain its guarded cause in logs");
            let detail = frame["attributes"]["error"].as_str().unwrap();
            assert!(detail.contains("private fixture pause detail"));
            assert!(!detail.contains(&token));
            assert_eq!(
                reply,
                format!(
                    "{}\n{failed}",
                    channel_runtime_cli_string("channel-runtime-stop-no-task")
                )
            );
            let replies = channel.sent_messages.lock().await;
            assert_eq!(
                replies
                    .iter()
                    .filter(|reply| reply.content.contains(&failed))
                    .count(),
                1
            );
            assert!(replies.iter().all(|reply| {
                !reply.content.contains("private fixture pause detail")
                    && reply.content != channel_runtime_cli_string("goal-mode-pause-pending")
            }));
            return;
        }
        let expected_status = match ending {
            GoalIngressEnding::Cancel => {
                message.content = "/goal cancel".into();
                TaskStatus::Cancelled
            }
            GoalIngressEnding::DeferredPause => {
                message.content = "/goal pause".into();
                TaskStatus::Paused
            }
            GoalIngressEnding::DeferredBudget => {
                message.content = "/goal budget set --tokens 10".into();
                TaskStatus::Paused
            }
            GoalIngressEnding::ImmediatePause | GoalIngressEnding::ImmediatePauseDrainFailure => {
                message.content = "/goal pause now".into();
                TaskStatus::Paused
            }
            GoalIngressEnding::Stop | GoalIngressEnding::StopDrainFailure => {
                message.content = "/stop".into();
                TaskStatus::Paused
            }
            GoalIngressEnding::CustodyDispose
            | GoalIngressEnding::StopAdmissionFailure
            | GoalIngressEnding::SettingsFailure
            | GoalIngressEnding::ClaimFailure
            | GoalIngressEnding::AuditFailure
            | GoalIngressEnding::AuditFailureCompetingStart
            | GoalIngressEnding::BindCustodyDispose
            | GoalIngressEnding::TerminalCustodyDispose
            | GoalIngressEnding::IdleRetire => unreachable!(),
        };
        sender.send(message.clone()).await.unwrap();
        if matches!(
            ending,
            GoalIngressEnding::DeferredPause | GoalIngressEnding::DeferredBudget
        ) {
            let (queued_key, intent_kind) = match ending {
                GoalIngressEnding::DeferredPause => ("goal-mode-queued-pause", "pause"),
                GoalIngressEnding::DeferredBudget => ("goal-mode-queued-budget", "set_budget"),
                GoalIngressEnding::Cancel
                | GoalIngressEnding::ImmediatePause
                | GoalIngressEnding::Stop
                | GoalIngressEnding::ImmediatePauseDrainFailure
                | GoalIngressEnding::StopDrainFailure
                | GoalIngressEnding::StopAdmissionFailure
                | GoalIngressEnding::SettingsFailure
                | GoalIngressEnding::ClaimFailure
                | GoalIngressEnding::AuditFailure
                | GoalIngressEnding::AuditFailureCompetingStart
                | GoalIngressEnding::CustodyDispose
                | GoalIngressEnding::BindCustodyDispose
                | GoalIngressEnding::TerminalCustodyDispose
                | GoalIngressEnding::IdleRetire => unreachable!(),
            };
            let queued_notice = channel_runtime_cli_string(queued_key);
            tokio::time::timeout(Duration::from_millis(250), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == queued_notice)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("normal control must acknowledge while the Goal parent remains held");
            let held = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(held.status, TaskStatus::Running);
            assert_eq!(held.execution_epoch, task.execution_epoch);
            let goal = registry.get_goal_task(&held.id).await.unwrap().unwrap();
            assert!(goal.pause_reason.is_none());
            assert!(goal.effective_token_limit.is_none());
            let intent = audit.query_row(
                "SELECT state, task_id, expected_epoch FROM goal_control_intents WHERE session_id = ?1 AND kind = ?2",
                [&key, intent_kind],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
            ).unwrap();
            assert_eq!(intent, ("pending".into(), held.id, held.execution_epoch));
            assert_eq!(provider.requests.lock().unwrap().len(), 2);
            provider.goal_release.notify_one();
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap()
                    .status
                    == expected_status
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the admitted control must reach its specified boundary");
        if matches!(
            ending,
            GoalIngressEnding::ImmediatePause
                | GoalIngressEnding::Stop
                | GoalIngressEnding::ImmediatePauseDrainFailure
                | GoalIngressEnding::StopDrainFailure
        ) {
            let pending_notice = channel_runtime_cli_string("goal-mode-pause-pending");
            tokio::time::timeout(Duration::from_millis(250), async {
                while !channel
                    .sent_messages
                    .lock()
                    .await
                    .iter()
                    .any(|reply| reply.content == pending_notice)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("immediate pause acknowledges before the admitted operation settles");
            let paused = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(paused.id, task.id);
            assert_eq!(paused.status, TaskStatus::Paused);
            assert_eq!(paused.execution_epoch, task.execution_epoch + 1);
            let goal = registry.get_goal_task(&paused.id).await.unwrap().unwrap();
            assert_eq!(
                goal.pause_reason,
                Some(zeroclaw_goal::GoalPauseReason::OperatorPaused)
            );
            assert!(goal.pending_call_id.is_some());
            assert_eq!(goal.pending_call_epoch, Some(task.execution_epoch));
            assert_eq!(provider.requests.lock().unwrap().len(), 2);
            assert!(channel.sent_messages.lock().await.iter().all(|reply| {
                reply.content != "Goal boundary candidate"
                    && !reply.content.contains("Goal paused.")
            }));
            let pause_intents: i64 = audit
                .query_row(
                    "SELECT COUNT(*) FROM goal_control_intents WHERE session_id = ?1 AND kind = 'pause'",
                    [&key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(pause_intents, 0, "immediate pause must not be deferred");
            if matches!(
                ending,
                GoalIngressEnding::ImmediatePauseDrainFailure | GoalIngressEnding::StopDrainFailure
            ) {
                // Remove only the extension after the real pause fence and
                // acknowledgement. The held worker then reaches a genuine
                // store error during settlement; no test-only error verdict
                // replaces the production drain path.
                assert_eq!(
                    audit
                        .execute("DELETE FROM goal_tasks WHERE task_id = ?1", [&paused.id])
                        .unwrap(),
                    1
                );
            }
        }
    };
    // Always release both provider gates before surfacing a failed assertion.
    let mut result = std::panic::AssertUnwindSafe(outcome).catch_unwind().await;
    if let Some(barrier) = bind_barrier.as_ref() {
        barrier.release.notify_one();
    }
    provider.ordinary_release.notify_one();
    if let Some(gate) = audit_gate.as_ref() {
        if result.is_ok() {
            let publication = async {
                tokio::time::timeout(Duration::from_secs(2), gate.reached.notified())
                    .await
                    .expect("failure must reach the pre-FIFO registry gate");
                let predecessor = registry
                    .current_goal_for_session(&key)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(predecessor.status, TaskStatus::Failed);
                assert_eq!(provider.requests.lock().unwrap().len(), 1);
                assert_eq!(
                    registry
                        .terminal_reason_for_session_goal(&predecessor.id, &key)
                        .await
                        .unwrap()
                        .as_deref(),
                    Some("executor_start_failed")
                );
                audit
                    .execute_batch("DROP TRIGGER fixture_refuse_goal_audit")
                    .unwrap();
                message.content = "/goal start --unlimited competing fixture objective".into();
                sender.send(message.clone()).await.unwrap();
                let queued = channel_runtime_cli_string("goal-mode-queued-start");
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let replies = channel.sent_messages.lock().await;
                        if replies
                            .iter()
                            .filter(|reply| reply.content == queued)
                            .count()
                            == 2
                        {
                            assert!(
                                replies
                                    .iter()
                                    .all(|reply| !reply.content.contains("Goal started."))
                            );
                            break;
                        }
                        drop(replies);
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("competing Start must acknowledge while the failure notice is gated");
                assert_eq!(
                    registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .unwrap()
                        .id,
                    predecessor.id
                );
                gate.release.notify_one();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !channel
                        .sent_messages
                        .lock()
                        .await
                        .iter()
                        .any(|reply| reply.content.contains("Goal started."))
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("successor must start after predecessor notice settlement");
                assert_ne!(
                    registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .unwrap()
                        .id,
                    predecessor.id
                );
                message.content = "/goal cancel".into();
                sender.send(message.clone()).await.unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while registry
                        .current_goal_for_session(&key)
                        .await
                        .unwrap()
                        .unwrap()
                        .status
                        != TaskStatus::Cancelled
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("fixture must cancel the successor before parent release");
            };
            result = std::panic::AssertUnwindSafe(publication)
                .catch_unwind()
                .await;
        }
        // Release even when the pre-fix second acknowledgement assertion fails.
        gate.release.notify_one();
    }
    provider.goal_release.notify_one();
    drop(sender);
    let drained = tokio::time::timeout(Duration::from_secs(3), &mut dispatch).await;
    if drained.is_err() {
        dispatch.abort();
    }
    if audit_gate.is_some()
        && let Err(panic) = result
    {
        if drained.is_err() {
            let _ = dispatch.await;
        }
        std::panic::resume_unwind(panic);
    }
    assert!(
        drained.is_ok(),
        "actual ingress must drain after fixture release"
    );
    drained.unwrap().unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    if matches!(ending, GoalIngressEnding::AuditFailureCompetingStart) {
        assert!(
            verifier_server
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        let replies = channel.sent_messages.lock().await;
        let queued = channel_runtime_cli_string("goal-mode-queued-start");
        let notice = channel_runtime_cli_string("goal-mode-failure-executor-start");
        let positions: Vec<_> = replies
            .iter()
            .enumerate()
            .filter_map(|(index, reply)| (reply.content == queued).then_some(index))
            .collect();
        assert_eq!(positions.len(), 2);
        let notices: Vec<_> = replies
            .iter()
            .enumerate()
            .filter_map(|(index, reply)| (reply.content == notice).then_some(index))
            .collect();
        assert_eq!(notices.len(), 1);
        let started = replies
            .iter()
            .position(|reply| reply.content.contains("Goal started."))
            .unwrap();
        assert!(positions[0] < positions[1] && positions[1] < notices[0] && notices[0] < started);
        let generic = channel_runtime_cli_string("goal-mode-command-failed");
        assert!(replies.iter().all(|reply| reply.content != generic
            && !reply.content.contains("private fixture audit detail")));
        assert!(
            foreground::foreground_lock(&context.persist_locks, &key)
                .try_lock()
                .is_ok()
        );
        assert!(
            foreground::goal_command_lock(&context.persist_locks, &key)
                .try_lock()
                .is_ok()
        );
        return;
    }
    if matches!(ending, GoalIngressEnding::AuditFailure) {
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        assert!(
            verifier_server
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        let failed = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(
            registry
                .terminal_reason_for_session_goal(&failed.id, &key)
                .await
                .unwrap()
                .as_deref(),
            Some("executor_start_failed")
        );
        assert_eq!(intent_state(), "abandoned");
        let reason: String = audit
            .query_row(
                "SELECT outcome_reason FROM goal_control_intents WHERE session_id = ?1",
                [&key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "control_dropped");
        let replies = channel.sent_messages.lock().await;
        let queued = channel_runtime_cli_string("goal-mode-queued-start");
        let notice = channel_runtime_cli_string("goal-mode-failure-executor-start");
        assert_eq!(
            replies
                .iter()
                .filter(|reply| reply.content == queued)
                .count(),
            1
        );
        assert_eq!(
            replies
                .iter()
                .filter(|reply| reply.content == notice)
                .count(),
            1
        );
        assert!(
            replies
                .iter()
                .position(|reply| reply.content == queued)
                .unwrap()
                < replies
                    .iter()
                    .position(|reply| reply.content == notice)
                    .unwrap()
        );
        let generic = channel_runtime_cli_string("goal-mode-command-failed");
        assert!(replies.iter().all(|reply| reply.content != generic
            && !reply.content.contains("Goal started.")
            && !reply.content.contains("private fixture audit detail")));
        assert!(
            foreground::foreground_lock(&context.persist_locks, &key)
                .try_lock()
                .is_ok()
        );
        assert!(
            foreground::goal_command_lock(&context.persist_locks, &key)
                .try_lock()
                .is_ok()
        );
        return;
    }
    if matches!(
        ending,
        GoalIngressEnding::SettingsFailure | GoalIngressEnding::ClaimFailure
    ) {
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        assert!(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            verifier_server
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(intent_state(), "abandoned");
        let reason: String = audit
            .query_row(
                "SELECT outcome_reason FROM goal_control_intents WHERE session_id = ?1",
                [&key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "control_dropped");
        let replies = channel.sent_messages.lock().await;
        let queued = channel_runtime_cli_string("goal-mode-queued-start");
        let failed = channel_runtime_cli_string("goal-mode-command-failed");
        assert_eq!(
            replies
                .iter()
                .filter(|reply| reply.content == queued)
                .count(),
            1
        );
        assert_eq!(
            replies
                .iter()
                .filter(|reply| reply.content == failed)
                .count(),
            1
        );
        assert!(
            replies
                .iter()
                .position(|reply| reply.content == queued)
                .unwrap()
                < replies
                    .iter()
                    .position(|reply| reply.content == failed)
                    .unwrap()
        );
        assert!(replies.iter().all(|reply| {
            !reply.content.contains("Goal started.")
                && !reply.content.contains("invalid Goal default limits")
                && !reply.content.contains("private fixture claim detail")
        }));
        return;
    }
    if matches!(ending, GoalIngressEnding::StopAdmissionFailure) {
        let scope = launched_scope.as_ref().unwrap();
        let current = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.id, scope.task_id());
        assert_eq!(current.status, TaskStatus::Cancelled);
        assert_eq!(current.execution_epoch, scope.execution_epoch() + 1);
        assert_eq!(provider.requests.lock().unwrap().len(), 2);
        assert!(!supervisor.owns_scope(scope).await);
        assert!(
            verifier_server
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        return;
    }
    if matches!(
        ending,
        GoalIngressEnding::ImmediatePauseDrainFailure | GoalIngressEnding::StopDrainFailure
    ) {
        let scope = launched_scope.as_ref().unwrap();
        let current = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.id, scope.task_id());
        assert_eq!(current.status, TaskStatus::Paused);
        assert_eq!(current.execution_epoch, scope.execution_epoch() + 1);
        assert_eq!(provider.requests.lock().unwrap().len(), 2);
        assert!(
            verifier_server
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        let replies = channel.sent_messages.lock().await;
        let pending = channel_runtime_cli_string("goal-mode-pause-pending");
        let failed = channel_runtime_cli_string("goal-mode-command-failed");
        assert_eq!(
            replies
                .iter()
                .filter(|reply| reply.content == pending)
                .count(),
            1
        );
        assert_eq!(
            replies
                .iter()
                .filter(|reply| reply.content == failed)
                .count(),
            1
        );
        assert!(
            replies
                .iter()
                .position(|reply| reply.content == pending)
                .unwrap()
                < replies
                    .iter()
                    .position(|reply| reply.content == failed)
                    .unwrap()
        );
        return;
    }
    if matches!(ending, GoalIngressEnding::IdleRetire) {
        let current = registry
            .current_goal_for_session(&key)
            .await
            .unwrap()
            .unwrap();
        let (id, epoch) = predecessor.unwrap();
        assert_eq!(current.id, id);
        assert_eq!(current.execution_epoch, epoch);
        assert_eq!(current.status, TaskStatus::Paused);
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        assert!(
            verifier_server
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(unsettled_intents(), 0);
        assert!(
            foreground::goal_supervisor_slot(&context.persist_locks, &key)
                .lock()
                .await
                .is_none()
        );
        return;
    }
    if matches!(
        ending,
        GoalIngressEnding::CustodyDispose
            | GoalIngressEnding::BindCustodyDispose
            | GoalIngressEnding::TerminalCustodyDispose
    ) {
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
        assert!(
            verifier_server
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(unsettled_intents(), 0);
        assert!(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .is_none()
        );
        return;
    }
    if matches!(ending, GoalIngressEnding::DeferredBudget) {
        // Unlike Pause/Cancel, a budget update does not fence and drain the
        // worker itself. Consume its completion through the real supervisor;
        // owns_scope deliberately remains true for an undrained result.
        let completion = tokio::time::timeout(
            Duration::from_secs(3),
            supervisor.drain(launched_scope.as_ref().unwrap()),
        )
        .await
        .expect("budget-limited worker must finish without verifier admission");
        // The producer's committed budget fence must survive the original
        // worker drain without converting the resumable pause into failure.
        assert_eq!(
            completion.unwrap(),
            zeroclaw_goal::GoalExecutionOutcome::Fenced
        );
        assert_eq!(
            registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Paused,
            "RG-55: draining a budget-exhausted worker must not terminalize its resumable pause",
        );
    }
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    assert!(
        verifier_server
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    if !matches!(ending, GoalIngressEnding::Cancel) {
        let scope = launched_scope.as_ref().unwrap();
        let goal = registry
            .get_goal_task(scope.task_id())
            .await
            .unwrap()
            .unwrap();
        let (reason, intent_kind) = match ending {
            GoalIngressEnding::DeferredPause => (
                zeroclaw_goal::GoalPauseReason::OperatorPaused,
                Some("pause"),
            ),
            GoalIngressEnding::DeferredBudget => {
                assert_eq!(goal.effective_token_limit, Some(10));
                (
                    zeroclaw_goal::GoalPauseReason::BudgetExhausted,
                    Some("set_budget"),
                )
            }
            GoalIngressEnding::ImmediatePause | GoalIngressEnding::Stop => {
                (zeroclaw_goal::GoalPauseReason::OperatorPaused, None)
            }
            GoalIngressEnding::Cancel
            | GoalIngressEnding::ImmediatePauseDrainFailure
            | GoalIngressEnding::StopDrainFailure
            | GoalIngressEnding::StopAdmissionFailure
            | GoalIngressEnding::SettingsFailure
            | GoalIngressEnding::ClaimFailure
            | GoalIngressEnding::AuditFailure
            | GoalIngressEnding::AuditFailureCompetingStart
            | GoalIngressEnding::CustodyDispose
            | GoalIngressEnding::BindCustodyDispose
            | GoalIngressEnding::TerminalCustodyDispose
            | GoalIngressEnding::IdleRetire => unreachable!(),
        };
        assert_eq!(goal.pause_reason, Some(reason));
        assert_eq!(
            goal.accounting_state,
            zeroclaw_goal::GoalAccountingState::Complete
        );
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert_eq!(
            tracker
                .get_usage_totals_for_task(scope.task_id())
                .unwrap()
                .0,
            12
        );
        if let Some(intent_kind) = intent_kind {
            assert_eq!(
                audit
                    .query_row(
                        "SELECT state FROM goal_control_intents WHERE session_id = ?1 AND kind = ?2",
                        [&key, intent_kind],
                        |row| row.get::<_, String>(0)
                    )
                    .unwrap(),
                "applied"
            );
        }
        assert_eq!(
            backend
                .load(&key)
                .iter()
                .filter(|row| row.role == "assistant" && row.content == "Goal boundary candidate")
                .count(),
            1
        );
        let replies = channel.sent_messages.lock().await;
        let candidate = replies
            .iter()
            .position(|reply| reply.content == "Goal boundary candidate")
            .unwrap();
        if matches!(
            ending,
            GoalIngressEnding::ImmediatePause | GoalIngressEnding::Stop
        ) {
            let pending_notice = channel_runtime_cli_string("goal-mode-pause-pending");
            assert_eq!(
                replies
                    .iter()
                    .filter(|reply| reply.content == pending_notice)
                    .count(),
                1
            );
            assert!(
                replies
                    .iter()
                    .position(|reply| reply.content == pending_notice)
                    .unwrap()
                    < candidate
            );
            let queued_notice = channel_runtime_cli_string("goal-mode-queued-pause");
            assert!(
                replies.iter().all(|reply| reply.content != queued_notice
                    && !reply.content.contains("Goal failed."))
            );
        }
        if matches!(ending, GoalIngressEnding::DeferredBudget) {
            let core_error = channel_user_error_message(
                &anyhow::Error::new(
                    zeroclaw_goal::accounting::GoalOperationFenced::BUDGET_EXHAUSTED,
                ),
                "Goal budget is exhausted",
            );
            assert_eq!(
                replies
                    .iter()
                    .filter(|reply| reply.content == core_error)
                    .count(),
                1,
                "the ordinary core error remains visible exactly once"
            );
            assert!(
                replies
                    .iter()
                    .all(|reply| !reply.content.contains("Goal failed."))
            );
            let budget_updated = channel_runtime_cli_string("goal-mode-budget-updated");
            let budget_updates = replies
                .iter()
                .filter(|reply| reply.content == budget_updated)
                .count();
            assert_eq!(budget_updates, 1);
            let final_response = replies
                .iter()
                .position(|reply| reply.content == budget_updated)
                .unwrap();
            assert!(
                candidate < final_response,
                "the budget response follows the settled parent candidate"
            );
        } else {
            let paused = replies
                .iter()
                .position(|reply| reply.content.contains("Goal paused."))
                .unwrap();
            assert_eq!(
                replies
                    .iter()
                    .filter(|reply| reply.content.contains("Goal paused."))
                    .count(),
                1
            );
            assert!(
                candidate < paused,
                "the original candidate precedes the final pause notice"
            );
        }
    }
    let scope = launched_scope.unwrap();
    assert!(!supervisor.owns_scope(&scope).await);
    if !matches!(ending, GoalIngressEnding::Cancel) {
        use zeroclaw_commands::goal::{GoalBudgetSelection, GoalCommand};
        let recovery = async {
            if matches!(ending, GoalIngressEnding::DeferredBudget) {
                let mut budget = goal_execution::submit_matrix_goal(
                    context.clone(),
                    key.clone(),
                    message.clone(),
                    GoalCommand::SetBudget(GoalBudgetSelection::Unlimited),
                )
                .await
                .unwrap();
                assert!(matches!(
                    budget.response(),
                    zeroclaw_goal::GoalResponse::BudgetUpdated(_)
                ));
                assert!(budget.after_response().await.unwrap().is_none());
                drop(budget);
            }
            message.content = "/goal resume".into();
            let mut resume = goal_execution::submit_matrix_goal(
                context.clone(),
                key.clone(),
                message.clone(),
                GoalCommand::Resume { response: None },
            )
            .await
            .unwrap();
            assert!(matches!(
                resume.response(),
                zeroclaw_goal::GoalResponse::Resumed(_)
            ));
            // Deliver the native lifecycle acknowledgement before the real
            // host handoff, just as the production delivery coordinator does.
            channel
                .send(&SendMessage::reply_to(
                    &message,
                    render_goal_response(resume.response()),
                ))
                .await
                .unwrap();
            assert!(resume.after_response().await.unwrap().is_none());
            drop(resume);
            tokio::time::timeout(Duration::from_secs(3), async {
                while provider.requests.lock().unwrap().len() < 3 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the cleanly paused Goal must resume into a real parent request");
            assert!(provider.requests.lock().unwrap()[2].iter().any(|row| {
                row.role == "assistant" && row.content == "Goal boundary candidate"
            }));
            let resumed = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(resumed.id, scope.task_id());
            assert_eq!(resumed.status, TaskStatus::Running);
            assert_eq!(resumed.execution_epoch, scope.execution_epoch() + 2);
            let resumed_scope = zeroclaw_goal::GoalExecutionScope::new(
                resumed.id,
                key.clone(),
                resumed.execution_epoch,
            )
            .unwrap();
            let mut cancel = goal_execution::submit_matrix_goal(
                context.clone(),
                key.clone(),
                message.clone(),
                GoalCommand::Cancel,
            )
            .await
            .unwrap();
            provider.resumed_release.notify_one();
            cancel.after_response().await.unwrap();
            assert!(!supervisor.owns_scope(&resumed_scope).await);
            let cancelled = registry
                .current_goal_for_session(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(cancelled.id, scope.task_id());
            assert_eq!(cancelled.status, TaskStatus::Cancelled);
            assert_eq!(cancelled.execution_epoch, scope.execution_epoch() + 3);
            let goal = registry
                .get_goal_task(&cancelled.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                goal.accounting_state,
                zeroclaw_goal::GoalAccountingState::Complete
            );
            assert!(goal.pending_call_id.is_none());
            assert!(goal.pending_call_epoch.is_none());
            assert_eq!(
                tracker
                    .get_usage_totals_for_task(scope.task_id())
                    .unwrap()
                    .0,
                24
            );
        };
        let result = std::panic::AssertUnwindSafe(recovery).catch_unwind().await;
        provider.resumed_release.notify_one();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        assert_eq!(provider.requests.lock().unwrap().len(), 3);
        assert!(
            verifier_server
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
    }
}

enum GoalInboundHook {
    Cancel,
    Rewrite,
}

#[async_trait::async_trait]
impl zeroclaw_runtime::hooks::HookHandler for GoalInboundHook {
    fn name(&self) -> &str {
        "goal-control-ingress-fixture"
    }

    async fn on_message_received(
        &self,
        mut message: ChannelMessage,
    ) -> zeroclaw_runtime::hooks::HookResult<ChannelMessage> {
        match self {
            Self::Cancel => {
                zeroclaw_runtime::hooks::HookResult::Cancel("fixture cancellation".into())
            }
            Self::Rewrite => {
                message.content = "rewritten ordinary request".into();
                message.sender = "@other:example.test".into();
                message.reply_target = "rewritten-room".into();
                zeroclaw_runtime::hooks::HookResult::Continue(message)
            }
        }
    }
}

async fn dispatch_goal_with_hook(hook: GoalInboundHook) -> Vec<SendMessage> {
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GatedProbeModelProvider::default());
    let mut context = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        matrix_control_test_config(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    let mut hooks = zeroclaw_runtime::hooks::HookRunner::new();
    hooks.register(Box::new(hook));
    Arc::get_mut(&mut context).unwrap().hooks = Some(Arc::new(hooks));
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let mut message = matrix_msg("@fixture:example.test");
    message.content = "/goal unknown".into();
    sender.send(message).await.unwrap();
    drop(sender);
    tokio::time::timeout(
        Duration::from_secs(3),
        run_message_dispatch_loop(receiver, AgentRouter::single(context), 4),
    )
    .await
    .unwrap();
    assert!(
        provider.entered_ids().is_empty(),
        "the hook must not turn a Goal command into a model prompt"
    );
    channel.sent_messages.lock().await.clone()
}

#[tokio::test]
async fn matrix_goal_ingress_keeps_hook_cancellation_before_admission() {
    assert!(
        dispatch_goal_with_hook(GoalInboundHook::Cancel)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn matrix_goal_ingress_keeps_original_command_and_reply_authority() {
    let replies = dispatch_goal_with_hook(GoalInboundHook::Rewrite).await;
    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].content,
        channel_runtime_cli_string("goal-mode-command-invalid")
    );
    assert_eq!(
        replies[0].recipient,
        matrix_msg("@fixture:example.test").reply_target
    );
}

#[test]
fn matrix_goal_ingress_classification_preserves_surface_and_passive_boundaries() {
    let mut message = matrix_msg("@fixture:example.test");
    for command in [
        "/goal start --unlimited fixture objective",
        "/goal resume first line\nsecond line",
        "/goal pause",
        "/goal pause now",
        "/goal cancel",
        "/goal status",
        "/goal unknown",
    ] {
        message.content = command.into();
        assert!(is_matrix_goal_control(&message), "{command}");
    }
    message.passive_context = true;
    assert!(!is_matrix_goal_control(&message));
    message.passive_context = false;
    message.channel = "cli".into();
    assert!(!is_matrix_goal_control(&message));
    message.channel = "matrix".into();
    message.content = "ordinary text mentioning /goal resume".into();
    assert!(!is_matrix_goal_control(&message));
}

#[test]
fn matrix_goal_queued_acknowledgements_name_the_control_boundary() {
    use zeroclaw_goal::{GoalQueuedControl, GoalResponse};
    for (control, expected) in [
        (
            GoalQueuedControl::Start,
            "Goal start queued; it begins after the current turn.",
        ),
        (
            GoalQueuedControl::Resume,
            "Goal resume queued; it continues after the current turn.",
        ),
        (
            GoalQueuedControl::Pause,
            "Goal pause queued; it applies after the current step.",
        ),
        (
            GoalQueuedControl::Budget,
            "Goal budget change queued; it applies after the current step.",
        ),
    ] {
        assert_eq!(
            render_goal_response(&GoalResponse::Queued(control)),
            expected
        );
    }
}

fn matrix_control_test_config() -> zeroclaw_config::schema::Config {
    let mut config = zeroclaw_config::schema::Config::default();
    config.channels.matrix.insert(
        "default".into(),
        zeroclaw_config::schema::MatrixConfig {
            interrupt_on_new_message: true,
            ..Default::default()
        },
    );
    config
}

fn matrix_control_test_ctx() -> Arc<ChannelRuntimeContext> {
    test_runtime_ctx_with_config_agent_and_provider_ref(
        Arc::new(SendMessageRecordingChannel::matrix()),
        Arc::new(GatedProbeModelProvider::default()),
        matrix_control_test_config(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    )
}

async fn assert_matrix_control_does_not_register(content: &str) {
    let ctx = matrix_control_test_ctx();
    let in_flight = Arc::new(Mutex::new(HashMap::new()));
    let sequence = Arc::new(AtomicU64::new(1));
    let generation = CancellationToken::new();
    let ordinary = matrix_msg("@fixture:example.test");
    let registration = register_inbound_turn(&ctx, &ordinary, &in_flight, &sequence, &generation)
        .await
        .unwrap();
    let mut control = ordinary.clone();
    control.content = content.into();
    let control_registration =
        register_inbound_turn(&ctx, &control, &in_flight, &sequence, &generation).await;

    assert!(
        !registration.cancellation.is_cancelled(),
        "a Matrix Goal control must not interrupt the admitted ordinary turn"
    );
    assert!(
        control_registration.is_none(),
        "a Matrix Goal control must not acquire an ordinary interruption slot"
    );
    assert_eq!(sequence.load(Ordering::Relaxed), 2);
    assert_eq!(
        in_flight
            .lock()
            .unwrap()
            .values()
            .map(Vec::len)
            .sum::<usize>(),
        1
    );
}

#[tokio::test]
async fn matrix_goal_normal_controls_do_not_register_or_interrupt() {
    for command in [
        "/goal start --unlimited fixture objective",
        "/goal resume",
        "/goal resume first line\nsecond line",
        "/goal pause",
    ] {
        assert_matrix_control_does_not_register(command).await;
    }
}

#[tokio::test]
async fn matrix_goal_parse_error_does_not_register_or_interrupt() {
    assert_matrix_control_does_not_register("/goal unknown").await;
}

#[tokio::test]
async fn matrix_goal_control_exemption_preserves_ordinary_interruption() {
    let ctx = matrix_control_test_ctx();
    let in_flight = Arc::new(Mutex::new(HashMap::new()));
    let sequence = Arc::new(AtomicU64::new(1));
    let generation = CancellationToken::new();
    let message = matrix_msg("@fixture:example.test");
    let first = register_inbound_turn(&ctx, &message, &in_flight, &sequence, &generation)
        .await
        .unwrap();
    let second = register_inbound_turn(&ctx, &message, &in_flight, &sequence, &generation)
        .await
        .unwrap();
    assert!(first.cancellation.is_cancelled());
    assert!(!second.cancellation.is_cancelled());
    assert_eq!(sequence.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn matrix_goal_parse_error_is_acknowledged_during_ordinary_turn_without_debounce() {
    let channel = Arc::new(SendMessageRecordingChannel::matrix());
    let provider = Arc::new(GatedProbeModelProvider::default());
    provider.hold("m1");
    let mut config = matrix_control_test_config();
    config.channels.debounce_ms = 1000;
    let ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel.clone(),
        provider.clone(),
        config,
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let mut dispatch =
        zeroclaw_spawn::spawn!(run_message_dispatch_loop(rx, AgentRouter::single(ctx), 4));
    let mut ordinary = matrix_msg("@fixture:example.test");
    ordinary.content = "message_id=m1 ordinary request".into();
    tx.send(ordinary.clone()).await.unwrap();
    let entered = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if provider.entered_ids().contains(&"m1".into()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    if entered.is_err() {
        provider.release("m1");
        dispatch.abort();
        panic!("the ordinary request must enter the provider before the control");
    }
    let mut control = ordinary;
    control.content = "/goal unknown".into();
    tx.send(control).await.unwrap();
    let expected = channel_runtime_cli_string("goal-mode-command-invalid");
    let acknowledged = tokio::time::timeout(Duration::from_millis(250), async {
        loop {
            if channel
                .sent_messages
                .lock()
                .await
                .iter()
                .any(|message| message.content == expected)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;

    // Always release the fixture before asserting, including the expected red.
    provider.release("m1");
    drop(tx);
    let drained = tokio::time::timeout(Duration::from_secs(3), &mut dispatch).await;
    if drained.is_err() {
        dispatch.abort();
    }
    assert!(
        drained.is_ok(),
        "the real dispatcher must drain after release"
    );
    drained.unwrap().unwrap();
    assert!(
        acknowledged.is_ok(),
        "a Goal parse error must bypass the one-second debounce and ordinary lane"
    );
    assert!(
        channel
            .sent_messages
            .lock()
            .await
            .iter()
            .any(|message| { message.content.contains("done for message_id=m1") }),
        "the control must leave the ordinary turn's response intact"
    );
    assert_eq!(provider.entered_ids(), ["m1"]);
}
