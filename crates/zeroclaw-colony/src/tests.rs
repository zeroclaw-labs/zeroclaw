use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;
use zeroclaw_config::colony::{ColonyConnection, ColonyRoom};
use zeroclaw_config::schema::AliasedAgentConfig;

#[derive(Default)]
struct RecordingRunner {
    calls: Mutex<Vec<(String, String)>>,
    started: Notify,
    release: Notify,
    hold_first: bool,
    require_approval: bool,
    count: AtomicUsize,
    outputs: Mutex<std::collections::VecDeque<String>>,
}

#[async_trait::async_trait]
impl ColonyTurnRunner for RecordingRunner {
    async fn run(
        &self,
        _config: Config,
        alias: &str,
        prompt: String,
        _admission: AgentExecutionAdmission,
        _read_only: bool,
        cancel: CancellationToken,
        channel: Option<Arc<dyn zeroclaw_api::channel::Channel>>,
    ) -> Result<String> {
        self.calls.lock().push((alias.to_string(), prompt));
        let call = self.count.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            self.started.notify_one();
            if self.require_approval {
                let channel = channel.context("missing approval channel")?;
                let decision = channel
                    .request_approval(
                        "operator",
                        &zeroclaw_api::channel::ChannelApprovalRequest {
                            tool_name: "shell".to_string(),
                            arguments_summary: "selected command".to_string(),
                            raw_arguments: Some(serde_json::json!({"command":"test-command"})),
                            position: None,
                        },
                    )
                    .await?;
                anyhow::ensure!(
                    decision == Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve),
                    "approval denied"
                );
            }
            if self.hold_first {
                tokio::select! { ()=self.release.notified()=>{},()=cancel.cancelled()=>anyhow::bail!("cancelled") }
            }
        }
        Ok(self
            .outputs
            .lock()
            .pop_front()
            .unwrap_or_else(|| format!("settled output from {alias}")))
    }
}

fn fixture(runner: Arc<RecordingRunner>) -> (tempfile::TempDir, Arc<ColonyRuntime>) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config {
        data_dir: dir.path().join("data"),
        config_path: dir.path().join("config.toml"),
        ..Default::default()
    };
    config
        .agents
        .insert("queen".to_string(), AliasedAgentConfig::default());
    config
        .agents
        .insert("worker".to_string(), AliasedAgentConfig::default());
    config
        .agents
        .insert("outside".to_string(), AliasedAgentConfig::default());
    config.colonies.insert(
        "team".to_string(),
        ColonyConfig {
            name: "Test team".to_string(),
            queen: "queen".to_string(),
            members: vec!["worker".to_string()],
            connections: vec![
                ColonyConnection {
                    from: "queen".to_string(),
                    to: "worker".to_string(),
                },
                ColonyConnection {
                    from: "worker".to_string(),
                    to: "queen".to_string(),
                },
            ],
            ..Default::default()
        },
    );
    let config = Arc::new(RwLock::new(config));
    let authority = crate_authority(&config);
    let mut runtime = ColonyRuntime::open(config, authority).unwrap();
    Arc::get_mut(&mut runtime).unwrap().runner = runner;
    (dir, runtime)
}

fn crate_authority(config: &Arc<RwLock<Config>>) -> AgentExecutionCapability {
    zeroclaw_runtime::live_config_authority::LiveConfigAuthority::from_config(Arc::clone(config))
        .execution_capability()
}

fn request() -> GoalRequest {
    GoalRequest {
        objective: "Prepare an output".to_string(),
        mode: GoalContextMode::Fresh,
        previous_goal_id: None,
        proposal: QueenProposal {
            questions: vec![],
            summary: "one specialist and Queen review".to_string(),
            assignments: vec![ColonyAssignment {
                agent: "worker".to_string(),
                instruction: "prepare selected output".to_string(),
            }],
            new_agents: vec![],
        },
        approve_new_agents: false,
        token_limit: None,
        cost_limit_usd: None,
    }
}

async fn settled(runtime: &ColonyRuntime, id: &str) -> GoalView {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if !runtime.active.lock().contains_key(id) {
                return runtime.goal(id).await.unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn wait_for_member_waiter(lock: &Arc<tokio::sync::Mutex<()>>) {
    // The map and this test retain two handles. A third is the selector
    // holding its handle while queued on the deliberately locked Queen.
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while Arc::strong_count(lock) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn check_selector_read_revocation(active_goal: bool) {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .rooms
        .push(ColonyRoom {
            id: "private".to_string(),
            name: "Private room".to_string(),
            readers: vec!["queen".to_string(), "worker".to_string()],
            publishers: vec!["worker".to_string()],
            responders: ColonyResponders::QueenSelected,
            ..Default::default()
        });
    let lock = runtime.member_lock("queen");
    let guard = lock.lock().await;
    let goal = if active_goal {
        let goal = runtime.create_goal("team", request()).await.unwrap();
        let received = runtime
            .send_message("team", "room:private", "private room content")
            .await
            .unwrap();
        assert_eq!(received.len(), 1);
        runtime.start(&goal.task.id).await.unwrap();
        Some(goal)
    } else {
        None
    };
    let direct = if active_goal {
        None
    } else {
        let runtime = Arc::clone(&runtime);
        Some(zeroclaw_spawn::spawn!(async move {
            runtime
                .send_message("team", "room:private", "private room content")
                .await
        }))
    };
    wait_for_member_waiter(&lock).await;
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .rooms[0]
        .readers
        .retain(|alias| alias != "queen");
    drop(guard);
    if let Some(goal) = goal {
        let paused = settled(&runtime, &goal.task.id).await;
        assert_eq!(paused.task.status, TaskStatus::Paused);
        assert_eq!(
            paused.goal.pause_reason,
            Some(GoalPauseReason::HumanEscalation)
        );
        assert!(paused.execution.active_child_id.is_none());
        assert!(
            runtime
                .store
                .pending_inbox(&goal.task.id)
                .unwrap()
                .is_some()
        );
        assert!(
            paused
                .messages
                .iter()
                .all(|message| message.sender == "user")
        );
    } else {
        let error = direct.unwrap().await.unwrap().unwrap_err();
        assert!(error.to_string().contains("selection permission changed"));
    }
    assert!(runner.calls.lock().is_empty());
}

#[tokio::test]
async fn inactive_room_selector_rechecks_read_permission_after_queen_lock() {
    check_selector_read_revocation(false).await;
}

#[tokio::test]
async fn active_inbox_selector_rechecks_read_permission_after_queen_lock() {
    check_selector_read_revocation(true).await;
}

#[tokio::test]
async fn room_selectors_rebuild_candidates_after_waiting_for_queen() {
    for active_goal in [false, true] {
        let runner = Arc::new(RecordingRunner::default());
        runner
            .outputs
            .lock()
            .push_back(serde_json::json!({"agent":"queen"}).to_string());
        let (_dir, runtime) = fixture(Arc::clone(&runner));
        runtime
            .config
            .write()
            .colonies
            .get_mut("team")
            .unwrap()
            .rooms
            .push(ColonyRoom {
                id: "private".to_string(),
                name: "Private room".to_string(),
                readers: vec!["queen".to_string(), "worker".to_string()],
                publishers: vec!["queen".to_string(), "worker".to_string()],
                responders: ColonyResponders::QueenSelected,
                ..Default::default()
            });
        let lock = runtime.member_lock("queen");
        let guard = lock.lock().await;
        let goal = if active_goal {
            let goal = runtime.create_goal("team", request()).await.unwrap();
            runtime
                .send_message("team", "room:private", "private room content")
                .await
                .unwrap();
            runtime.start(&goal.task.id).await.unwrap();
            Some(goal)
        } else {
            None
        };
        let direct = if active_goal {
            None
        } else {
            let runtime = Arc::clone(&runtime);
            Some(zeroclaw_spawn::spawn!(async move {
                runtime
                    .send_message("team", "room:private", "private room content")
                    .await
            }))
        };
        wait_for_member_waiter(&lock).await;
        runtime
            .config
            .write()
            .colonies
            .get_mut("team")
            .unwrap()
            .rooms[0]
            .publishers
            .retain(|alias| alias != "worker");
        drop(guard);
        if let Some(goal) = goal {
            assert_eq!(
                settled(&runtime, &goal.task.id).await.task.status,
                TaskStatus::Completed
            );
        } else {
            assert_eq!(direct.unwrap().await.unwrap().unwrap().len(), 2);
        }
        let calls = runner.calls.lock();
        assert_eq!(calls[0].0, "queen");
        assert!(calls[0].1.contains("[\"queen\"]"));
        assert!(!calls[0].1.contains("\"worker\""));
    }
}

#[tokio::test]
async fn generic_config_publication_cannot_delete_owner_of_paused_goal() {
    let (_dir, runtime) = fixture(Arc::new(RecordingRunner::default()));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    assert_eq!(goal.task.status, TaskStatus::Paused);
    assert!(goal.execution.active_child_id.is_none());
    let old = runtime.config.read().clone();
    let mut deleted = old.clone();
    deleted.colonies.remove("team");
    assert!(runtime.validate_config_change(&old, &deleted).is_err());
    runtime.cancel(&goal.task.id).await.unwrap();
    assert!(runtime.validate_config_change(&old, &deleted).is_ok());
}

#[tokio::test]
async fn goal_admission_waits_for_owner_deletion_publication_before_inserting_task() {
    let (_dir, runtime) = fixture(Arc::new(RecordingRunner::default()));
    let lock = zeroclaw_config::write_lock::shared_config_write_lock();
    let writer = lock.lock().await;
    let old = runtime.config.read().clone();
    let mut next = old.clone();
    next.colonies.remove("team");
    runtime.validate_config_change(&old, &next).unwrap();
    let creator = {
        let runtime = Arc::clone(&runtime);
        zeroclaw_spawn::spawn!(async move { runtime.create_goal("team", request()).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while runtime.goal_admission.try_lock().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // The old owner is still visible during durable save, but admission must
    // wait for publication rather than creating a task from that old snapshot.
    assert!(runtime.config.read().colonies.contains_key("team"));
    assert!(runtime.store.colony_goal_ids("team").unwrap().is_empty());
    next.mark_dirty("colonies");
    next.save_dirty().await.unwrap();
    *runtime.config.write() = next;
    drop(writer);
    assert!(
        creator
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("colony_not_found")
    );
    assert!(runtime.store.colony_goal_ids("team").unwrap().is_empty());
    assert!(
        runtime
            .store
            .list_by_agent("queen")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn disabled_queen_and_specialist_are_rejected_before_model_dispatch() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    runtime
        .config
        .write()
        .agents
        .get_mut("queen")
        .unwrap()
        .enabled = false;
    let error = runtime
        .clarify("team", "Prepare work", &[])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("disabled"));
    assert!(runner.calls.lock().is_empty());
    runtime
        .config
        .write()
        .agents
        .get_mut("queen")
        .unwrap()
        .enabled = true;
    runtime
        .config
        .write()
        .agents
        .get_mut("worker")
        .unwrap()
        .enabled = false;
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    let paused = settled(&runtime, &goal.task.id).await;
    assert_eq!(paused.task.status, TaskStatus::Paused);
    assert!(paused.execution.active_child_id.is_none());
    assert!(paused.goal.pause_description.unwrap().contains("disabled"));
    assert!(runner.calls.lock().is_empty());
}

#[tokio::test]
async fn disabled_room_responder_is_not_dispatched() {
    for active_goal in [false, true] {
        let runner = Arc::new(RecordingRunner::default());
        let (_dir, runtime) = fixture(Arc::clone(&runner));
        runtime
            .config
            .write()
            .agents
            .get_mut("worker")
            .unwrap()
            .enabled = false;
        runtime
            .config
            .write()
            .colonies
            .get_mut("team")
            .unwrap()
            .rooms
            .push(ColonyRoom {
                id: "review".to_string(),
                name: "Review".to_string(),
                readers: vec!["worker".to_string()],
                publishers: vec!["worker".to_string()],
                responders: ColonyResponders::Addressed,
                ..Default::default()
            });
        if active_goal {
            let goal = runtime.create_goal("team", request()).await.unwrap();
            runtime
                .send_message("team", "room:review", "@worker Review this")
                .await
                .unwrap();
            runtime.start(&goal.task.id).await.unwrap();
            assert_eq!(
                settled(&runtime, &goal.task.id).await.task.status,
                TaskStatus::Paused
            );
        } else {
            assert!(
                runtime
                    .send_message("team", "room:review", "@worker Review this")
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("disabled")
            );
        }
        assert!(runner.calls.lock().is_empty());
    }
}

#[tokio::test]
async fn goal_runs_real_dispatch_boundary_with_canonical_child_settlement() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    assert_eq!(goal.task.status, TaskStatus::Paused);
    assert!(runtime.create_goal("team", request()).await.is_err());
    runtime.start(&goal.task.id).await.unwrap();
    let finished = settled(&runtime, &goal.task.id).await;
    assert_eq!(finished.task.status, TaskStatus::Completed);
    assert_eq!(finished.messages.len(), 2);
    assert_eq!(
        runner
            .calls
            .lock()
            .iter()
            .map(|(a, _)| a.as_str())
            .collect::<Vec<_>>(),
        vec!["worker", "queen"]
    );
    let children = runtime.store.list_by_agent("worker").await.unwrap();
    assert_eq!(
        children[0].parent_id.as_deref(),
        Some(goal.task.id.as_str())
    );
    assert_eq!(children[0].status, TaskStatus::Completed);
}

#[tokio::test]
async fn missing_return_direction_blocks_dispatch_without_implicit_reply() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .connections
        .truncate(1);
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    let paused = settled(&runtime, &goal.task.id).await;
    assert_eq!(paused.task.status, TaskStatus::Paused);
    assert!(
        paused
            .goal
            .pause_description
            .unwrap()
            .contains("return connection")
    );
    assert!(runner.calls.lock().is_empty());
}

#[tokio::test]
async fn pause_settles_current_turn_and_resume_does_not_repeat_it() {
    let runner = Arc::new(RecordingRunner {
        hold_first: true,
        ..Default::default()
    });
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    runner.started.notified().await;
    let paused = runtime.pause(&goal.task.id).await.unwrap();
    assert!(paused.execution.active_child_id.is_some());
    let old = runtime.config.read().clone();
    let mut next = old.clone();
    next.colonies.get_mut("team").unwrap().members.clear();
    next.colonies.get_mut("team").unwrap().connections.clear();
    assert!(runtime.validate_config_change(&old, &next).is_err());
    runner.release.notify_one();
    let paused = settled(&runtime, &goal.task.id).await;
    assert_eq!(paused.task.status, TaskStatus::Paused);
    assert_eq!(paused.execution.next_assignment, 1);
    assert!(paused.execution.active_child_id.is_none());
    assert!(runtime.validate_config_change(&old, &next).is_ok());
    runtime.resume(&goal.task.id).await.unwrap();
    assert_eq!(
        settled(&runtime, &goal.task.id).await.task.status,
        TaskStatus::Completed
    );
    assert_eq!(runner.calls.lock().len(), 2);
}

#[tokio::test]
async fn cancellation_wins_over_inflight_result_and_keeps_team() {
    let runner = Arc::new(RecordingRunner {
        hold_first: true,
        ..Default::default()
    });
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    runner.started.notified().await;
    let cancelled = runtime.cancel(&goal.task.id).await.unwrap();
    assert_eq!(cancelled.task.status, TaskStatus::Cancelled);
    assert!(cancelled.messages.is_empty());
    assert!(runtime.config.read().colonies.contains_key("team"));
    assert!(runtime.resume(&goal.task.id).await.is_err());
}

#[tokio::test]
async fn continue_imports_selected_goal_while_fresh_excludes_its_context() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let prior = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&prior.task.id).await.unwrap();
    let prior = settled(&runtime, &prior.task.id).await;
    let mut continued = request();
    continued.mode = GoalContextMode::Continue;
    continued.previous_goal_id = Some(prior.task.id.clone());
    let continued = runtime.create_goal("team", continued).await.unwrap();
    let context = runtime
        .turn_context(&continued, "worker", "queen")
        .await
        .unwrap();
    assert!(context.contains("settled output from worker"));
    runtime.cancel(&continued.task.id).await.unwrap();
    let fresh = runtime.create_goal("team", request()).await.unwrap();
    assert_eq!(
        runtime
            .turn_context(&fresh, "worker", "queen")
            .await
            .unwrap(),
        "[]"
    );
    assert_eq!(
        runtime.goal(&prior.task.id).await.unwrap().messages.len(),
        2
    );
}

#[tokio::test]
async fn supervised_restart_waits_and_autonomous_never_replays_uncertain_turn() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime
        .store
        .resume_goal_task(&goal.task.id, 999_999, "dead-owner", None)
        .await
        .unwrap();
    assert!(
        !runtime
            .store
            .reconcile_lost(&goal.task.id, "new-boot")
            .await
            .unwrap()
    );
    runtime.recover().await.unwrap();
    let recovered = runtime.goal(&goal.task.id).await.unwrap();
    assert_eq!(recovered.task.status, TaskStatus::Paused);
    assert_eq!(
        recovered.goal.pause_reason,
        Some(GoalPauseReason::DaemonRestart)
    );
    assert!(runner.calls.lock().is_empty());
    runtime
        .store
        .resume_goal_task(&goal.task.id, 999_999, "dead-owner", None)
        .await
        .unwrap();
    let mut execution = recovered.execution;
    execution.active_child_id = Some("interrupted-child".to_string());
    let mut child = runtime.task_record(
        "interrupted-child".to_string(),
        "worker".to_string(),
        TaskKind::Subagent,
        Some(goal.task.id.clone()),
    );
    child.owner_pid = 999_999;
    child.owner_boot_id = "dead-owner".to_string();
    runtime.store.begin_colony_turn(child, &execution).unwrap();
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .autonomy = ColonyAutonomy::Autonomous;
    runtime.recover().await.unwrap();
    assert!(runtime.resume(&goal.task.id).await.is_err());
    assert!(runner.calls.lock().is_empty());
    runtime.reconcile_turn(&goal.task.id, false).await.unwrap();
    runtime.resume(&goal.task.id).await.unwrap();
    assert_eq!(
        settled(&runtime, &goal.task.id).await.task.status,
        TaskStatus::Completed
    );
    assert_eq!(runner.calls.lock()[0].0, "queen");
}

#[tokio::test]
async fn tool_approval_is_exact_once_and_does_not_change_profile() {
    let runner = Arc::new(RecordingRunner {
        require_approval: true,
        ..Default::default()
    });
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    runner.started.notified().await;
    let approval = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let goal = runtime.goal(&goal.task.id).await.unwrap();
            if let Some(a) = goal.approvals.first() {
                break a.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        approval.arguments,
        serde_json::json!({"command":"test-command"})
    );
    runtime
        .approve_tool(&goal.task.id, &approval.id, true)
        .await
        .unwrap();
    assert!(
        runtime
            .approve_tool(&goal.task.id, &approval.id, true)
            .await
            .is_err()
    );
    assert_eq!(
        settled(&runtime, &goal.task.id).await.task.status,
        TaskStatus::Completed
    );
}

#[tokio::test]
async fn addressed_rooms_do_not_wake_unaddressed_members() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .rooms
        .push(ColonyRoom {
            id: "review".to_string(),
            name: "Review".to_string(),
            readers: vec!["worker".to_string()],
            publishers: vec!["worker".to_string()],
            ..Default::default()
        });
    assert_eq!(
        runtime
            .send_message("team", "room:review", "Hello room")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(runner.calls.lock().is_empty());
    assert_eq!(
        runtime
            .send_message("team", "room:review", "@worker review this")
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(runner.calls.lock()[0].0, "worker");
    assert!(
        runtime
            .send_message("team", "outside", "not permitted")
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_queen_and_specialist_provider_turns_preserve_exact_tool_approval_and_goal_usage() {
    use axum::{Json, Router, extract::State, routing::post};
    use zeroclaw_config::schema::{
        ModelProviderConfig, OllamaModelProviderConfig, RiskProfileConfig, RuntimeProfileConfig,
    };
    async fn respond(State(calls): State<Arc<AtomicUsize>>) -> Json<serde_json::Value> {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        let message = match call {
            0 => serde_json::json!({"content":serde_json::to_string(&request().proposal).unwrap()}),
            1 => {
                serde_json::json!({"content":null,"tool_calls":[{"id":"write-test-output","type":"function",
                "function":{"name":"file_write","arguments":"{\"path\":\"colony-output.txt\",\"content\":\"approved output\"}"}}]})
            }
            2 => serde_json::json!({"content":"The approved file was written"}),
            _ => serde_json::json!({"content":"Goal completed with the specialist's saved output"}),
        };
        Json(
            serde_json::json!({"choices":[{"message":message,"finish_reason":"stop"}],
            "usage":{"prompt_tokens":5,"completion_tokens":5,"total_tokens":10}}),
        )
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/v1/chat/completions", post(respond))
        .with_state(Arc::clone(&calls));
    let server =
        zeroclaw_spawn::spawn!(async move { axum::serve(listener, router).await.unwrap() });
    let (_dir, mut runtime) = fixture(Arc::new(RecordingRunner::default()));
    let runtime_store = Arc::clone(&runtime.store);
    Arc::get_mut(&mut runtime).unwrap().runner = Arc::new(RuntimeTurnRunner {
        store: runtime_store,
    });
    {
        let mut config = runtime.config.write();
        config.providers.models.ollama.insert(
            "test".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("colony-test".to_string()),
                    timeout_secs: Some(5),
                    uri: Some(format!("http://{addr}")),
                    pricing: HashMap::from([
                        ("colony-test.input".to_string(), 1.0),
                        ("colony-test.output".to_string(), 1.0),
                    ]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        config.risk_profiles.insert(
            "test".to_string(),
            RiskProfileConfig {
                always_ask: vec!["file_write".to_string()],
                ..Default::default()
            },
        );
        config.runtime_profiles.insert(
            "test".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 4,
                ..Default::default()
            },
        );
        for alias in ["queen", "worker"] {
            let agent = config.agents.get_mut(alias).unwrap();
            agent.model_provider = "ollama.test".into();
            agent.risk_profile = "test".into();
            agent.runtime_profile = "test".into();
        }
        config.cost.enabled = true;
    }
    let proposal = runtime
        .clarify("team", "Prepare an output", &[])
        .await
        .unwrap();
    let mut task = request();
    task.proposal = proposal;
    task.token_limit = Some(1000);
    let goal = runtime.create_goal("team", task).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    let approval = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let current = runtime.goal(&goal.task.id).await.unwrap();
            if let Some(approval) = current.approvals.first() {
                break approval.clone();
            }
            assert_eq!(
                current.task.status,
                TaskStatus::Running,
                "goal paused before approval: {:?}",
                current.goal
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let output = runtime
        .config
        .read()
        .agent_workspace_dir("worker")
        .join("colony-output.txt");
    assert!(!output.exists(), "tool executed before approval");
    assert_eq!(approval.tool, "file_write");
    runtime
        .approve_tool(&goal.task.id, &approval.id, true)
        .await
        .unwrap();
    let finished = settled(&runtime, &goal.task.id).await;
    assert_eq!(
        finished.task.status,
        TaskStatus::Completed,
        "{:?}",
        finished.goal
    );
    assert_eq!(std::fs::read_to_string(output).unwrap(), "approved output");
    let config = runtime.config.read();
    let tracker = zeroclaw_runtime::cost::CostTracker::get_or_init_global(
        config.cost.clone(),
        &config.data_dir,
    )
    .unwrap();
    assert!(tracker.get_usage_totals_for_task(&goal.task.id).unwrap().0 >= 30);
    assert!(calls.load(Ordering::SeqCst) >= 4);
    server.abort();
}

#[tokio::test]
async fn paused_room_inbox_does_not_select_until_goal_owned_resume() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .rooms
        .push(ColonyRoom {
            id: "jobs".to_string(),
            name: "Jobs".to_string(),
            readers: vec!["worker".to_string()],
            publishers: vec!["worker".to_string()],
            responders: ColonyResponders::Addressed,
            ..Default::default()
        });
    let goal = runtime.create_goal("team", request()).await.unwrap();
    let received = runtime
        .send_message("team", "room:jobs", "@worker Check this posting")
        .await
        .unwrap();
    assert_eq!(received.len(), 1);
    assert!(runner.calls.lock().is_empty());
    assert_eq!(
        runtime.goal(&goal.task.id).await.unwrap().task.status,
        TaskStatus::Paused
    );
    runtime.start(&goal.task.id).await.unwrap();
    let complete = settled(&runtime, &goal.task.id).await;
    assert_eq!(complete.task.status, TaskStatus::Completed);
    assert!(
        complete
            .messages
            .iter()
            .any(|m| m.sender == "worker"
                && m.in_reply_to.as_deref() == Some(received[0].id.as_str()))
    );
    let children = runtime.store.list_by_agent("worker").await.unwrap();
    assert_eq!(children.len(), 2);
    assert!(
        children
            .iter()
            .all(|t| t.parent_id.as_deref() == Some(goal.task.id.as_str()))
    );
}

#[tokio::test]
async fn cancelled_goal_never_flushes_paused_direct_inbox() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime
        .send_message("team", "worker", "A new requirement")
        .await
        .unwrap();
    runtime.cancel(&goal.task.id).await.unwrap();
    assert!(runtime.resume(&goal.task.id).await.is_err());
    assert!(runner.calls.lock().is_empty());
}

fn refinement(
    questions: Vec<String>,
    agent: &str,
    new_agents: Vec<ColonyAgentProposal>,
) -> QueenProposal {
    QueenProposal {
        questions,
        summary: "Further specialist work".to_string(),
        assignments: vec![ColonyAssignment {
            agent: agent.to_string(),
            instruction: "Refine the settled result".to_string(),
        }],
        new_agents,
    }
}
fn planned_runner(outputs: Vec<String>) -> Arc<RecordingRunner> {
    Arc::new(RecordingRunner {
        outputs: Mutex::new(outputs.into()),
        ..Default::default()
    })
}

#[tokio::test]
async fn supervised_refinement_requires_explicit_batch_confirmation() {
    let plan = refinement(vec![], "worker", vec![]);
    let runner = planned_runner(vec![
        "initial output".into(),
        serde_json::to_string(&plan).unwrap(),
        "refined output".into(),
        "Final result".into(),
    ]);
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    let proposed = settled(&runtime, &goal.task.id).await;
    assert_eq!(proposed.task.status, TaskStatus::Paused);
    assert!(proposed.execution.pending_plan.is_some());
    assert!(runtime.resume(&goal.task.id).await.is_err());
    assert_eq!(runner.calls.lock().len(), 2);
    runtime.confirm_plan(&goal.task.id).await.unwrap();
    let done = settled(&runtime, &goal.task.id).await;
    assert_eq!(done.task.status, TaskStatus::Completed);
    assert_eq!(done.execution.plan_rounds, 1);
    assert_eq!(runner.calls.lock().len(), 4);
}

#[tokio::test]
async fn grouped_goal_clarification_is_owned_and_still_requires_plan_review() {
    let question_plan = refinement(vec!["Which output format?".to_string()], "worker", vec![]);
    let ready = refinement(vec![], "worker", vec![]);
    let runner = planned_runner(vec![
        "initial output".into(),
        serde_json::to_string(&question_plan).unwrap(),
        serde_json::to_string(&ready).unwrap(),
        "refined output".into(),
        "Final result".into(),
    ]);
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    settled(&runtime, &goal.task.id).await;
    runtime.pause(&goal.task.id).await.unwrap();
    let answers = vec![ClarificationAnswer {
        question: "Which output format?".into(),
        answer: "Plain text".into(),
    }];
    assert!(runtime.clarify_goal(&goal.task.id, &answers).await.is_err());
    assert_eq!(runner.calls.lock().len(), 2);
    runtime.resume(&goal.task.id).await.unwrap();
    let review = runtime.clarify_goal(&goal.task.id, &answers).await.unwrap();
    assert_eq!(review.task.status, TaskStatus::Paused);
    assert!(review.execution.pending_plan.unwrap().questions.is_empty());
    assert!(runner.calls.lock()[2].1.contains("Plain text"));
    runtime.confirm_plan(&goal.task.id).await.unwrap();
    assert_eq!(
        settled(&runtime, &goal.task.id).await.task.status,
        TaskStatus::Completed
    );
    let queen_children = runtime.store.list_by_agent("queen").await.unwrap();
    assert!(
        queen_children
            .iter()
            .filter(|t| t.kind == TaskKind::Subagent)
            .all(|t| t.parent_id.as_deref() == Some(goal.task.id.as_str()))
    );
}

#[tokio::test]
async fn autonomous_refinement_creates_only_declared_specialist_and_directions() {
    let agent = ColonyAgentProposal {
        alias: "specialist".into(),
        core_command: "Refine prepared outputs".into(),
        template: "worker".into(),
        connections: vec![
            ColonyConnection {
                from: "queen".into(),
                to: "specialist".into(),
            },
            ColonyConnection {
                from: "specialist".into(),
                to: "queen".into(),
            },
        ],
    };
    let plan = refinement(vec![], "specialist", vec![agent]);
    let runner = planned_runner(vec![
        "initial output".into(),
        serde_json::to_string(&plan).unwrap(),
        "specialist output".into(),
        "Final result".into(),
    ]);
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .autonomy = ColonyAutonomy::Autonomous;
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    let done = settled(&runtime, &goal.task.id).await;
    assert_eq!(done.task.status, TaskStatus::Completed, "{:?}", done.goal);
    let config = runtime.config.read();
    assert!(
        config.colonies["team"]
            .members
            .iter()
            .any(|a| a == "specialist")
    );
    assert!(config.colony_allows_communication("queen", "specialist"));
    assert!(config.colony_allows_communication("specialist", "queen"));
    assert!(!config.colony_allows_communication("worker", "specialist"));
    assert_eq!(runner.calls.lock()[2].0, "specialist");
}

#[tokio::test]
async fn refinement_assignment_and_round_limits_stop_before_unbounded_work() {
    let mut oversized = refinement(vec![], "worker", vec![]);
    oversized.assignments = vec![oversized.assignments[0].clone(); 64];
    let runner = planned_runner(vec![
        "initial output".into(),
        serde_json::to_string(&oversized).unwrap(),
    ]);
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    let blocked = settled(&runtime, &goal.task.id).await;
    assert_eq!(blocked.task.status, TaskStatus::Paused);
    assert!(
        blocked
            .goal
            .pause_description
            .unwrap()
            .contains("planning limit")
    );
    assert_eq!(runner.calls.lock().len(), 2);
    let plan = serde_json::to_string(&refinement(vec![], "worker", vec![])).unwrap();
    let outputs = (0..9)
        .flat_map(|_| ["output".to_string(), plan.clone()])
        .collect();
    let runner = planned_runner(outputs);
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .autonomy = ColonyAutonomy::Autonomous;
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime.start(&goal.task.id).await.unwrap();
    let blocked = settled(&runtime, &goal.task.id).await;
    assert_eq!(blocked.task.status, TaskStatus::Paused);
    assert_eq!(blocked.execution.plan_rounds, 8);
    assert_eq!(runner.calls.lock().len(), 18);
}

#[tokio::test]
async fn fresh_queued_conversation_excludes_previous_goal_and_continue_selects_it() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let prior = runtime.create_goal("team", request()).await.unwrap();
    runtime
        .send_message("team", "worker", "PRIVATE_PRIOR_CONTEXT")
        .await
        .unwrap();
    runtime.start(&prior.task.id).await.unwrap();
    settled(&runtime, &prior.task.id).await;
    let fresh = runtime.create_goal("team", request()).await.unwrap();
    runtime
        .send_message("team", "worker", "CURRENT_FRESH_CONTEXT")
        .await
        .unwrap();
    let first = runner.calls.lock().len();
    runtime.start(&fresh.task.id).await.unwrap();
    settled(&runtime, &fresh.task.id).await;
    assert!(
        runner.calls.lock()[first..]
            .iter()
            .all(|(_, prompt)| !prompt.contains("PRIVATE_PRIOR_CONTEXT"))
    );
    let mut continued = request();
    continued.mode = GoalContextMode::Continue;
    continued.previous_goal_id = Some(prior.task.id.clone());
    let next = runtime.create_goal("team", continued).await.unwrap();
    let first = runner.calls.lock().len();
    runtime.start(&next.task.id).await.unwrap();
    settled(&runtime, &next.task.id).await;
    assert!(
        runner.calls.lock()[first..]
            .iter()
            .any(|(_, prompt)| prompt.contains("PRIVATE_PRIOR_CONTEXT"))
    );
}

#[tokio::test]
async fn uncertain_usage_survives_controller_reopen_and_cannot_be_cleared_by_resume() {
    use zeroclaw_runtime::execution_scope::ExecutionScopeObserver;
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let mut task = request();
    task.token_limit = Some(100);
    let goal = runtime.create_goal("team", task).await.unwrap();
    let scope = GoalScope {
        id: goal.task.id.clone(),
        store: Arc::clone(&runtime.store),
        config: Arc::clone(&runtime.config),
        persistence_error: Arc::new(Mutex::new(None)),
        approval_required: Arc::new(Mutex::new(None)),
    };
    scope.record_usage_error(&anyhow::Error::msg("UNREPORTED_BILLABLE_ATTEMPT"));
    let mut reopened =
        ColonyRuntime::open(Arc::clone(&runtime.config), runtime.capability.clone()).unwrap();
    Arc::get_mut(&mut reopened).unwrap().runner = Arc::clone(&runner) as Arc<dyn ColonyTurnRunner>;
    reopened.start(&goal.task.id).await.unwrap();
    let blocked = settled(&reopened, &goal.task.id).await;
    assert_eq!(
        blocked.goal.pause_reason,
        Some(GoalPauseReason::BudgetUnavailable)
    );
    assert!(
        blocked
            .goal
            .pause_description
            .unwrap()
            .contains("UNREPORTED_BILLABLE_ATTEMPT")
    );
    assert!(runner.calls.lock().is_empty());
    reopened.resume(&goal.task.id).await.unwrap();
    assert_eq!(
        settled(&reopened, &goal.task.id).await.task.status,
        TaskStatus::Paused
    );
    assert!(runner.calls.lock().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_goal_admission_commits_one_canonical_goal_only() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(runner);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let attempts = (0..2)
        .map(|_| {
            let runtime = Arc::clone(&runtime);
            let barrier = Arc::clone(&barrier);
            zeroclaw_spawn::spawn!(async move {
                barrier.wait().await;
                runtime.create_goal("team", request()).await
            })
        })
        .collect::<Vec<_>>();
    let mut successes = 0;
    let mut rejections = 0;
    for attempt in attempts {
        match attempt.await.unwrap() {
            Ok(_) => successes += 1,
            Err(error) => {
                assert!(error.to_string().contains("already has an active goal"));
                rejections += 1;
            }
        }
    }
    assert_eq!((successes, rejections), (1, 1));
    assert_eq!(runtime.store.colony_goal_ids("team").unwrap().len(), 1);
    let canonical_goals = runtime
        .store
        .list_by_agent("queen")
        .await
        .unwrap()
        .into_iter()
        .filter(|task| task.kind == TaskKind::Goal)
        .collect::<Vec<_>>();
    assert_eq!(canonical_goals.len(), 1);
    assert_eq!(canonical_goals[0].status, TaskStatus::Paused);
}

#[tokio::test]
async fn reviewed_batch_allows_declared_new_member_edges_without_alias_takeover() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(runner);
    let plan = refinement(
        vec![],
        "first",
        vec![
            ColonyAgentProposal {
                alias: "first".into(),
                core_command: "Find postings".into(),
                template: "worker".into(),
                connections: vec![ColonyConnection {
                    from: "first".into(),
                    to: "second".into(),
                }],
            },
            ColonyAgentProposal {
                alias: "second".into(),
                core_command: "Review postings".into(),
                template: "worker".into(),
                connections: vec![ColonyConnection {
                    from: "second".into(),
                    to: "third".into(),
                }],
            },
            ColonyAgentProposal {
                alias: "third".into(),
                core_command: "Prepare resumes".into(),
                template: "worker".into(),
                connections: vec![],
            },
        ],
    );
    runtime.validate_proposal("team", &plan).unwrap();
    runtime
        .apply_agent_proposals_authorized("team", &plan, true)
        .await
        .unwrap();
    {
        let config = runtime.config.read();
        assert!(config.colony_allows_communication("first", "second"));
        assert!(config.colony_allows_communication("second", "third"));
        assert!(!config.colony_allows_communication("second", "first"));
        assert!(!config.colony_allows_communication("queen", "first"));
        assert!(!config.colony_allows_communication("third", "outside"));
    }
    let mut takeover = plan.clone();
    takeover.new_agents[0].core_command = "Different purpose".into();
    assert!(
        runtime
            .apply_agent_proposals_authorized("team", &takeover, true)
            .await
            .is_err()
    );
    assert_eq!(
        runtime.config.read().agent("first").unwrap().core_command,
        "Find postings"
    );
    let mut duplicate = plan.clone();
    duplicate.new_agents.push(plan.new_agents[0].clone());
    assert!(runtime.validate_proposal("team", &duplicate).is_err());
}

#[tokio::test]
async fn initial_review_requires_batch_consent_and_passes_clarified_success_criteria() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let agent = ColonyAgentProposal {
        alias: "new_worker".into(),
        core_command: "Prepare reviewed output".into(),
        template: "worker".into(),
        connections: vec![
            ColonyConnection {
                from: "queen".into(),
                to: "new_worker".into(),
            },
            ColonyConnection {
                from: "new_worker".into(),
                to: "queen".into(),
            },
        ],
    };
    let mut task = request();
    task.proposal = refinement(vec![], "new_worker", vec![agent]);
    task.proposal.summary = "Salary at least 100000; remote only; prepare before applying".into();
    assert!(runtime.create_goal("team", task.clone()).await.is_err());
    assert!(runtime.config.read().agent("new_worker").is_none());
    assert!(runtime.goals("team").await.unwrap().is_empty());
    task.approve_new_agents = true;
    let goal = runtime.create_goal("team", task).await.unwrap();
    assert!(runtime.config.read().agent("new_worker").is_some());
    assert_eq!(goal.task.status, TaskStatus::Paused);
    runtime.start(&goal.task.id).await.unwrap();
    assert_eq!(
        settled(&runtime, &goal.task.id).await.task.status,
        TaskStatus::Completed
    );
    assert!(
        runner.calls.lock()[0]
            .1
            .contains("Salary at least 100000; remote only; prepare before applying")
    );
}

#[tokio::test]
async fn scoped_peer_publication_uses_one_direction_and_revocation_hides_prior_content() {
    use zeroclaw_runtime::execution_scope::ExecutionScopeObserver;
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(Arc::clone(&runner));
    let goal = runtime.create_goal("team", request()).await.unwrap();
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .connections
        .retain(|edge| edge.from == "worker");
    let scope = GoalScope {
        id: goal.task.id.clone(),
        store: Arc::clone(&runtime.store),
        config: Arc::clone(&runtime.config),
        persistence_error: Arc::new(Mutex::new(None)),
        approval_required: Arc::new(Mutex::new(None)),
    };
    let message_id = scope
        .publish_peer_message("worker", "queen", "ONE_WAY_FINDING")
        .unwrap()
        .unwrap();
    let view = runtime.goal(&goal.task.id).await.unwrap();
    assert!(
        view.messages
            .iter()
            .any(|m| m.id == message_id && m.sender == "worker" && m.recipient == "queen")
    );
    assert!(
        runtime
            .turn_context(&view, "queen", "queen")
            .await
            .unwrap()
            .contains("ONE_WAY_FINDING")
    );
    assert!(
        scope
            .publish_peer_message("queen", "worker", "No implicit reverse")
            .is_err()
    );
    assert!(
        scope
            .publish_peer_message("worker", "outside", "Outside denied")
            .is_err()
    );
    runtime
        .config
        .write()
        .colonies
        .get_mut("team")
        .unwrap()
        .connections
        .clear();
    assert!(
        !runtime
            .turn_context(&view, "queen", "queen")
            .await
            .unwrap()
            .contains("ONE_WAY_FINDING")
    );
    assert!(
        scope
            .publish_peer_message("worker", "queen", "Revoked")
            .is_err()
    );
    assert!(runner.calls.lock().is_empty());
    runtime.cancel(&goal.task.id).await.unwrap();
    assert!(
        scope
            .publish_peer_message("queen", "queen", "Cancelled")
            .is_err()
    );
}

#[tokio::test]
async fn operator_sender_cannot_be_impersonated_by_a_proposed_agent_alias() {
    let runner = Arc::new(RecordingRunner::default());
    let (_dir, runtime) = fixture(runner);
    let plan = refinement(
        vec![],
        "user",
        vec![ColonyAgentProposal {
            alias: "user".into(),
            core_command: "Pretend operator".into(),
            template: "worker".into(),
            connections: vec![],
        }],
    );
    assert!(
        runtime
            .validate_proposal("team", &plan)
            .unwrap_err()
            .to_string()
            .contains("reserved")
    );
    let mut request = request();
    request.proposal = plan;
    request.approve_new_agents = true;
    assert!(runtime.create_goal("team", request).await.is_err());
    assert!(runtime.config.read().agent("user").is_none());
    assert!(runtime.goals("team").await.unwrap().is_empty());
}
