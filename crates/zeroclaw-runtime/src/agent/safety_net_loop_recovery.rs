//! Scripted providers deliberately disregard correction to prove containment
//! at the same Agent APIs consumed by embedded callers and ACP.

use super::*;
use zeroclaw_api::model_provider::ConversationMessage;

type Requests = Arc<parking_lot::Mutex<Vec<Vec<ChatMessage>>>>;

struct RecordingProvider {
    scripted: ScriptedProvider,
    requests: Requests,
    steering: Option<mpsc::Sender<String>>,
}

impl zeroclaw_api::attribution::Attributable for RecordingProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Attributable::role(&self.scripted)
    }
    fn alias(&self) -> &str {
        "loop-recovery-test"
    }
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    async fn chat_with_system(
        &self,
        system: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> Result<String> {
        self.scripted
            .chat_with_system(system, message, model, temperature)
            .await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> Result<ChatResponse> {
        let first_request = {
            let mut requests = self.requests.lock();
            requests.push(request.messages.to_vec());
            requests.len() == 1
        };
        if first_request && let Some(steering) = &self.steering {
            steering
                .send("continue working".into())
                .await
                .expect("queue steering");
        }
        self.scripted.chat(request, model, temperature).await
    }
}

struct OutcomeTool {
    name: &'static str,
    calls: Arc<AtomicUsize>,
    recover_at: Option<usize>,
    changing_output: bool,
    success: bool,
    return_error: bool,
}

zeroclaw_api::tool_attribution!(OutcomeTool, zeroclaw_api::attribution::ToolKind::Plugin);

#[async_trait]
impl Tool for OutcomeTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "scripted fetch"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, args: serde_json::Value) -> Result<crate::tools::ToolResult> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.return_error {
            anyhow::bail!("failed fetch");
        }
        let success =
            self.success || self.recover_at == Some(attempt) || args["url"] == "alternate";
        Ok(crate::tools::ToolResult {
            success,
            output: if self.changing_output {
                format!("result {attempt}")
            } else {
                "same result".into()
            }
            .into(),
            error: (!success).then(|| "failed fetch".into()),
        })
    }
}

fn script(rounds: usize) -> Vec<ChatResponse> {
    (0..rounds)
        .map(|round| tool_response(vec![tool_call(&format!("fetch-{round}"), "fetch")]))
        .collect()
}

fn agent_for(
    responses: Vec<ChatResponse>,
    tool: OutcomeTool,
    pacing: Option<zeroclaw_config::schema::PacingConfig>,
) -> (TestAgent, Requests) {
    agent_for_with_steering(responses, tool, pacing, None)
}

fn agent_for_with_steering(
    responses: Vec<ChatResponse>,
    tool: OutcomeTool,
    pacing: Option<zeroclaw_config::schema::PacingConfig>,
    steering: Option<mpsc::Sender<String>>,
) -> (TestAgent, Requests) {
    let requests = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut agent = build_agent(
        Box::new(RecordingProvider {
            scripted: ScriptedProvider::new(responses),
            requests: Arc::clone(&requests),
            steering,
        }),
        vec![Box::new(tool)],
    );
    if let Some(pacing) = pacing {
        let config = Config {
            pacing,
            ..Config::default()
        };
        agent.provider_switch_config = Some(ProviderSwitchConfig {
            config: Some(Arc::new(config)),
            ..ProviderSwitchConfig::default()
        });
    }
    (agent, requests)
}

fn failing(calls: &Arc<AtomicUsize>) -> OutcomeTool {
    OutcomeTool {
        name: "fetch",
        calls: Arc::clone(calls),
        recover_at: None,
        changing_output: false,
        success: false,
        return_error: false,
    }
}

#[derive(Default)]
struct HookCounts {
    before: AtomicUsize,
    after: AtomicUsize,
    abandoned: AtomicUsize,
}

struct TestHook {
    counts: Arc<HookCounts>,
    cancel: bool,
}

#[async_trait]
impl crate::hooks::HookHandler for TestHook {
    fn name(&self) -> &str {
        "loop-recovery-test"
    }
    async fn before_tool_call(
        &self,
        name: String,
        args: serde_json::Value,
    ) -> crate::hooks::HookResult<(String, serde_json::Value)> {
        self.counts.before.fetch_add(1, Ordering::SeqCst);
        if self.cancel {
            crate::hooks::HookResult::Cancel("policy denied".into())
        } else {
            crate::hooks::HookResult::Continue((name, args))
        }
    }
    async fn on_after_tool_call(
        &self,
        _tool: &str,
        _result: &crate::tools::ToolResult,
        _duration: std::time::Duration,
    ) {
        self.counts.after.fetch_add(1, Ordering::SeqCst);
    }
    async fn on_tool_call_abandoned(
        &self,
        _context: &crate::hooks::ToolCallHookContext,
        _tool: &str,
    ) {
        self.counts.abandoned.fetch_add(1, Ordering::SeqCst);
    }
}

fn attach_hook(agent: &mut Agent, counts: &Arc<HookCounts>, cancel: bool) {
    let mut runner = crate::hooks::HookRunner::new();
    runner.register(Box::new(TestHook {
        counts: Arc::clone(counts),
        cancel,
    }));
    agent.hook_runner = Some(Arc::new(runner));
}

async fn run(agent: &mut Agent, streamed: bool) -> (String, Vec<TurnEvent>) {
    if streamed {
        let (tx, mut rx) = mpsc::channel(256);
        let result = agent
            .turn_streamed_with_steering_state("fetch it", tx, None, None)
            .await
            .expect("streamed turn completes");
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        (result.response, events)
    } else {
        (
            agent.turn("fetch it").await.expect("turn completes"),
            Vec::new(),
        )
    }
}

fn result_ids(agent: &Agent) -> Vec<String> {
    agent
        .history
        .iter()
        .flat_map(|message| match message {
            ConversationMessage::ToolResults(results) => results
                .iter()
                .map(|result| result.tool_call_id.clone())
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

#[tokio::test]
async fn repeated_failures_close_both_agent_apis_after_correction_and_preserve_pairs() {
    for streamed in [false, true] {
        for return_error in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut tool = failing(&calls);
            tool.return_error = return_error;
            let (mut agent, requests) = agent_for(script(8), tool, Some(Default::default()));
            let (response, events) = run(&mut agent, streamed).await;
            assert_eq!(calls.load(Ordering::SeqCst), 4);
            let correction = crate::i18n::get_required_cli_string("turn-repeated-failure-recovery");
            let stop = crate::i18n::get_required_cli_string("turn-repeated-failure-exhausted");
            let requests = requests.lock();
            assert_eq!(requests.len(), 4, "no summary request after exhaustion");
            assert!(requests[..3].iter().all(|request| {
                request
                    .iter()
                    .all(|message| !message.content.contains(&correction))
            }));
            assert!(
                requests[3].iter().any(
                    |message| message.role == "system" && message.content.contains(&correction)
                )
            );
            assert!(response.contains(&stop));
            assert_eq!(
                result_ids(&agent.agent),
                ["fetch-0", "fetch-1", "fetch-2", "fetch-3"]
            );
            if streamed {
                let started: Vec<_> = events
                    .iter()
                    .filter_map(|event| {
                        if let TurnEvent::ToolCall { id, .. } = event {
                            Some(id)
                        } else {
                            None
                        }
                    })
                    .collect();
                let completed: Vec<_> = events
                    .iter()
                    .filter_map(|event| {
                        if let TurnEvent::ToolResult { id, .. } = event {
                            Some(id)
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(started, completed);
                assert_eq!(completed.len(), 4);
                assert!(events.iter().any(
                    |event| matches!(event, TurnEvent::Chunk { delta } if delta.contains(&stop))
                ));
            }
        }
    }
}

#[tokio::test]
async fn warning_batch_does_not_consume_recovery_and_final_batch_results_survive() {
    for parallel in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let responses = vec![
            tool_response(
                (0..5)
                    .map(|i| tool_call(&format!("burst-{i}"), "fetch"))
                    .collect(),
            ),
            tool_response(vec![
                tool_call("last", "fetch"),
                tool_call("extra-1", "fetch"),
                tool_call("extra-2", "fetch"),
            ]),
            text_response("must not be requested"),
        ];
        let (mut agent, requests) = agent_for(responses, failing(&calls), Some(Default::default()));
        agent.config.resolved.parallel_tools = parallel;
        let counts = Arc::new(HookCounts::default());
        attach_hook(&mut agent, &counts, false);
        let (response, events) = run(&mut agent, true).await;
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        assert_eq!(requests.lock().len(), 2);
        assert!(response.contains(&crate::i18n::get_required_cli_string(
            "turn-repeated-failure-exhausted"
        )));
        assert_eq!(
            result_ids(&agent.agent).len(),
            8,
            "skipped recovery calls retain paired synthetic results"
        );
        assert_eq!(counts.before.load(Ordering::SeqCst), 8);
        assert_eq!(counts.after.load(Ordering::SeqCst), 6);
        assert_eq!(counts.abandoned.load(Ordering::SeqCst), 2);
        let skipped = crate::i18n::get_required_cli_string("turn-repeated-failure-retry-skipped");
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(event, TurnEvent::ToolResult { output, .. } if output == &skipped)
                })
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn recovery_retry_changed_url_and_other_useful_batch_work_continue() {
    for streamed in [false, true] {
        for parallel in [false, true] {
            for scenario in [0, 1, 2] {
                let calls = Arc::new(AtomicUsize::new(0));
                let mut responses = script(4);
                let mut tool = failing(&calls);
                if scenario == 0 {
                    tool.recover_at = Some(4);
                } else {
                    let mut alternate = tool_call("alternate", "fetch");
                    alternate.arguments = serde_json::json!({"url":"alternate"}).to_string();
                    if scenario == 1 {
                        responses[3] = tool_response(vec![alternate]);
                    } else {
                        responses[3].tool_calls.push(tool_call("extra", "fetch"));
                        responses[3].tool_calls.push(alternate);
                    }
                }
                responses.push(text_response("recovered"));
                let (mut agent, requests) = agent_for(responses, tool, Some(Default::default()));
                agent.config.resolved.parallel_tools = parallel;
                let (response, _) = run(&mut agent, streamed).await;
                assert!(response.ends_with("recovered"));
                assert!(!response.contains(&crate::i18n::get_required_cli_string(
                    "turn-repeated-failure-exhausted"
                )));
                assert_eq!(requests.lock().len(), 5);
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    if scenario == 2 { 5 } else { 4 }
                );
            }
        }
    }
}

#[tokio::test]
async fn successful_polling_is_advisory_and_changing_results_continue() {
    for changing_output in [false, true] {
        for streamed in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut responses = script(6);
            responses.push(text_response("poll complete"));
            let mut tool = failing(&calls);
            tool.success = true;
            tool.changing_output = changing_output;
            let (mut agent, requests) = agent_for(
                responses,
                tool,
                Some(zeroclaw_config::schema::PacingConfig {
                    loop_detection_min_elapsed_secs: Some(0),
                    ..Default::default()
                }),
            );
            let (response, _) = run(&mut agent, streamed).await;
            let advisory = crate::i18n::get_required_cli_string("turn-repeated-success-advisory");
            assert_eq!(
                response.matches(&advisory).count(),
                usize::from(!changing_output)
            );
            assert!(response.ends_with("poll complete"));
            assert_eq!(calls.load(Ordering::SeqCst), 6);
            assert_eq!(requests.lock().len(), 7);
        }
    }
}

#[tokio::test]
async fn disabled_and_standalone_pacing_preserve_chains_and_live_policy_refreshes_next_turn() {
    for streamed in [false, true] {
        for standalone in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut responses = script(5);
            responses.push(text_response("done"));
            responses.extend(script(8));
            let disabled = zeroclaw_config::schema::PacingConfig {
                loop_detection_enabled: false,
                ..Default::default()
            };
            let (mut agent, requests) = agent_for(
                responses,
                failing(&calls),
                (!standalone).then_some(disabled.clone()),
            );
            let live = Arc::new(parking_lot::RwLock::new(Config::default()));
            live.write().pacing = disabled;
            if !standalone {
                agent.provider_switch_config.as_mut().unwrap().live_config =
                    Some(Arc::clone(&live));
            }
            assert_eq!(run(&mut agent, streamed).await.0, "done");
            assert_eq!(calls.load(Ordering::SeqCst), 5);
            if !standalone {
                live.write().pacing.loop_detection_enabled = true;
                let response = run(&mut agent, streamed).await.0;
                assert!(response.contains(&crate::i18n::get_required_cli_string(
                    "turn-repeated-failure-exhausted"
                )));
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    9,
                    "fresh turn gets a fresh recovery budget"
                );
                assert_eq!(requests.lock().len(), 10);
            }
        }
    }
}

#[tokio::test]
async fn hook_approval_and_unavailable_results_do_not_count_as_executed_failures() {
    for streamed in [false, true] {
        for exclusion in ["hook", "approval", "unavailable"] {
            let calls = Arc::new(AtomicUsize::new(0));
            // Approval has its own prompt-repeat guard. One denial with a
            // threshold of one proves it cannot trigger execution recovery.
            let rounds = if exclusion == "approval" { 1 } else { 5 };
            let mut responses = script(rounds);
            if exclusion == "unavailable" {
                for response in &mut responses {
                    response.tool_calls[0].name = "missing".into();
                }
            }
            responses.push(text_response("done"));
            let (mut agent, requests) = agent_for(
                responses,
                failing(&calls),
                Some(zeroclaw_config::schema::PacingConfig {
                    loop_detection_max_repeats: 1,
                    ..Default::default()
                }),
            );
            let counts = Arc::new(HookCounts::default());
            if exclusion == "hook" {
                attach_hook(&mut agent, &counts, true);
            }
            if exclusion == "approval" {
                let profile = zeroclaw_config::schema::RiskProfileConfig {
                    level: crate::security::AutonomyLevel::Full,
                    always_ask: vec!["fetch".into()],
                    ..Default::default()
                };
                agent.approval_manager = Some(Arc::new(
                    crate::approval::ApprovalManager::for_non_interactive(&profile),
                ));
            }
            let (response, _) = run(&mut agent, streamed).await;
            assert_eq!(
                response, "done",
                "{exclusion} does not emit repetition correction"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(requests.lock().len(), rounds + 1);
            assert_eq!(result_ids(&agent.agent).len(), rounds);
            if exclusion == "hook" {
                assert_eq!(counts.abandoned.load(Ordering::SeqCst), rounds);
                assert_eq!(counts.after.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[tokio::test]
async fn streaming_exhaustion_is_terminal_despite_queued_steering() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (steer_tx, mut steer_rx) = mpsc::channel(4);
    let (mut agent, requests) = agent_for_with_steering(
        script(8),
        failing(&calls),
        Some(Default::default()),
        Some(steer_tx),
    );
    let (tx, _rx) = mpsc::channel(256);
    let result = agent
        .turn_streamed_with_steering_state("fetch it", tx, None, Some(&mut steer_rx))
        .await
        .expect("recoverable exhaustion completes");
    assert!(
        result
            .response
            .contains(&crate::i18n::get_required_cli_string(
                "turn-repeated-failure-exhausted"
            ))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        requests.lock().len(),
        4,
        "queued steering cannot restart an exhausted turn"
    );
    assert_eq!(steer_rx.try_recv().unwrap(), "continue working");
}

#[tokio::test]
async fn steering_reentry_preserves_advisory_and_failed_recovery_evidence() {
    for success in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut responses = script(3);
        responses.push(text_response("first round complete"));
        responses.extend(script(3));
        responses.push(text_response("second round complete"));
        let mut tool = failing(&calls);
        tool.success = success;
        let (steer_tx, mut steer_rx) = mpsc::channel(4);
        let (mut agent, requests) =
            agent_for_with_steering(responses, tool, Some(Default::default()), Some(steer_tx));
        let (tx, _rx) = mpsc::channel(256);
        let result = agent
            .turn_streamed_with_steering_state("fetch it", tx, None, Some(&mut steer_rx))
            .await
            .expect("steered turn completes");
        if success {
            assert_eq!(calls.load(Ordering::SeqCst), 6);
            let advisory = crate::i18n::get_required_cli_string("turn-repeated-success-advisory");
            assert_eq!(
                result.response.matches(&advisory).count(),
                1,
                "one advisory for the API turn"
            );
            assert_eq!(requests.lock().len(), 8);
        } else {
            assert_eq!(
                calls.load(Ordering::SeqCst),
                4,
                "reentry does not replenish recovery"
            );
            assert!(
                result
                    .response
                    .contains(&crate::i18n::get_required_cli_string(
                        "turn-repeated-failure-exhausted"
                    ))
            );
            assert_eq!(requests.lock().len(), 5);
        }
    }
}

#[tokio::test]
async fn activated_aliases_share_recovery_admission_and_canonical_ignore() {
    for return_error in [false, true] {
        for ignored in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let responses = vec![
                tool_response(vec![tool_call("alias-0", "fetch")]),
                tool_response(vec![tool_call("alias-1", "server__fetch")]),
                tool_response(vec![tool_call("alias-2", "fetch")]),
                tool_response(vec![
                    tool_call("alias-3", "server__fetch"),
                    tool_call("alias-4", "fetch"),
                    tool_call("alias-5", "server__fetch"),
                ]),
                text_response("done"),
            ];
            let pacing = zeroclaw_config::schema::PacingConfig {
                loop_ignore_tools: if ignored {
                    vec!["server__fetch".into()]
                } else {
                    Vec::new()
                },
                ..Default::default()
            };
            let (mut agent, requests) = agent_for(responses, failing(&calls), Some(pacing));
            agent.tools = crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(Vec::new());
            let mut tool = failing(&calls);
            tool.name = "server__fetch";
            tool.return_error = return_error;
            let mut activated = crate::tools::ActivatedToolSet::new();
            activated.activate("server__fetch".into(), Arc::new(tool));
            agent.activated_tools = Some(Arc::new(std::sync::Mutex::new(activated)));
            agent.config.resolved.parallel_tools = true;

            let (response, events) = run(&mut agent, true).await;
            assert_eq!(calls.load(Ordering::SeqCst), if ignored { 6 } else { 4 });
            assert_eq!(requests.lock().len(), if ignored { 5 } else { 4 });
            assert_eq!(
                result_ids(&agent.agent),
                [
                    "alias-0", "alias-1", "alias-2", "alias-3", "alias-4", "alias-5"
                ]
            );
            let requested_names: Vec<_> = agent
                .history
                .iter()
                .flat_map(|message| match message {
                    ConversationMessage::AssistantToolCalls { tool_calls, .. } => {
                        tool_calls.iter().map(|call| call.name.as_str()).collect()
                    }
                    _ => Vec::new(),
                })
                .collect();
            assert_eq!(
                requested_names,
                [
                    "fetch",
                    "server__fetch",
                    "fetch",
                    "server__fetch",
                    "fetch",
                    "server__fetch"
                ]
            );
            if ignored {
                assert_eq!(
                    response, "done",
                    "canonical ignore covers short aliases too"
                );
            } else {
                assert!(response.contains(&crate::i18n::get_required_cli_string(
                    "turn-repeated-failure-exhausted"
                )));
                let skipped =
                    crate::i18n::get_required_cli_string("turn-repeated-failure-retry-skipped");
                assert_eq!(
                    events.iter().filter(|event| {
                        matches!(event, TurnEvent::ToolResult { output, .. } if output == &skipped)
                    }).count(),
                    2
                );
            }
        }
    }
}
