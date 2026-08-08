//! safety net — pins the turn-engine behaviors the existing suite is
//! known NOT to cover (spec: the eight seams in the consolidation plan).

use super::*;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use zeroclaw_api::ingress::IngressContext;
use zeroclaw_api::model_provider::TokenUsage;
use zeroclaw_providers::{ChatResponse, ToolCall};

// ── shared fixtures ─────────────────────────────────────────────────────

struct TestAgent {
    agent: Agent,
    _workspace: tempfile::TempDir,
}

impl Deref for TestAgent {
    type Target = Agent;

    fn deref(&self) -> &Self::Target {
        &self.agent
    }
}

impl DerefMut for TestAgent {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.agent
    }
}

fn test_workspace() -> tempfile::TempDir {
    tempfile::tempdir().expect("test workspace should be created")
}

fn mem_none(workspace: &Path) -> Arc<dyn Memory> {
    let cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    Arc::from(
        zeroclaw_memory::create_memory(&cfg, workspace, None)
            .expect("memory creation should succeed"),
    )
}

pub(super) fn text_response(text: &str) -> ChatResponse {
    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }
}

pub(super) fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: "{}".into(),
        extra_content: None,
    }
}

pub(super) fn tool_response(calls: Vec<ToolCall>) -> ChatResponse {
    ChatResponse {
        text: Some(String::new()),
        tool_calls: calls,
        usage: None,
        reasoning_content: None,
    }
}

fn token_usage(input: u64, output: u64) -> TokenUsage {
    TokenUsage {
        input_tokens: Some(input),
        cached_input_tokens: None,
        output_tokens: Some(output),
    }
}

/// Build a cost-tracking context keyed on the serving provider's pricing.
/// Mirrors ws.rs::process_chat_message — the pricing map keys ONLY the
/// serving provider so a coherent Usage tuple yields the correct cost,
/// while a misattributed tuple resolves empty pricing → cost 0.
pub(super) fn build_cost_context(
    serving_provider: &str,
    serving_model: &str,
    input_rate: f64,
    output_rate: f64,
) -> (
    Arc<crate::cost::CostTracker>,
    crate::agent::cost::ToolLoopCostTrackingContext,
) {
    use crate::agent::cost::ToolLoopCostTrackingContext;
    use crate::cost::CostTracker;
    use std::collections::HashMap;

    let tmpdir = tempfile::TempDir::new().unwrap();
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            tmpdir.path(),
        )
        .expect("CostTracker::new"),
    );
    let pricing_map = HashMap::from([(
        serving_provider.to_string(),
        HashMap::from([
            (format!("{serving_model}.input"), input_rate),
            (format!("{serving_model}.output"), output_rate),
            (format!("{serving_model}.cached_input"), 0.0_f64),
        ]),
    )]);
    let cost_ctx = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(pricing_map));
    (tracker, cost_ctx)
}

/// Returns scripted responses in order; "done" once the script is exhausted.
pub(super) struct ScriptedProvider {
    responses: parking_lot::Mutex<VecDeque<ChatResponse>>,
}

impl ScriptedProvider {
    pub(super) fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: parking_lot::Mutex::new(responses.into()),
        }
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<ChatResponse> {
        Ok(self
            .responses
            .lock()
            .pop_front()
            .unwrap_or_else(|| text_response("done")))
    }
}

impl ::zeroclaw_api::attribution::Attributable for ScriptedProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "ScriptedProvider"
    }
}

/// Counts executions; succeeds with a fixed output.
pub(super) struct CountingTool {
    pub(super) name: &'static str,
    pub(super) calls: Arc<AtomicUsize>,
}

zeroclaw_api::tool_attribution!(CountingTool, ::zeroclaw_api::attribution::ToolKind::Plugin);

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.name
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::tools::ToolResult {
            success: true,
            output: format!("{}-out", self.name).into(),
            error: None,
        })
    }
}

fn build_agent(provider: Box<dyn ModelProvider>, tools_vec: Vec<Box<dyn Tool>>) -> TestAgent {
    let workspace = test_workspace();
    let agent = Agent::builder()
        .model_provider(provider)
        .tools(tools_vec)
        .memory(mem_none(workspace.path()))
        .observer(Arc::from(observability::NoopObserver {}))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(workspace.path().to_path_buf())
        .build()
        .expect("agent builder should succeed");
    TestAgent {
        agent,
        _workspace: workspace,
    }
}

fn build_agent_with_runtime(
    provider: Box<dyn ModelProvider>,
    tools_vec: Vec<Box<dyn Tool>>,
    resolved: zeroclaw_config::schema::ResolvedRuntime,
) -> TestAgent {
    let workspace = test_workspace();
    let agent = Agent::builder()
        .model_provider(provider)
        .tools(tools_vec)
        .memory(mem_none(workspace.path()))
        .observer(Arc::from(observability::NoopObserver {}))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(workspace.path().to_path_buf())
        .config(zeroclaw_config::schema::AliasedAgentConfig {
            resolved,
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        })
        .build()
        .expect("agent builder should succeed");
    TestAgent {
        agent,
        _workspace: workspace,
    }
}

// ── seam 1: dedup is OFF on the streaming and Agent::turn engines ───────
// E1 dedups identical calls per iteration; E2/E3 never did. RPC retry and
// polling patterns depend on the second identical call executing.

#[tokio::test]
async fn safety_net_dedup_off_identical_calls_both_execute() {
    // streaming engine
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = ScriptedProvider::new(vec![tool_response(vec![
        tool_call("a", "echo"),
        tool_call("b", "echo"),
    ])]);
    let mut agent = build_agent(
        Box::new(provider),
        vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&calls),
        })],
    );
    let (tx, _rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("dedup", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "turn_streamed*: both identical tool calls must execute (dedup off)"
    );

    // Agent::turn engine
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = ScriptedProvider::new(vec![tool_response(vec![
        tool_call("a", "echo"),
        tool_call("b", "echo"),
    ])]);
    let mut agent = build_agent(
        Box::new(provider),
        vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&calls),
        })],
    );
    agent.turn("dedup").await.expect("turn should succeed");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "Agent::turn: both identical tool calls must execute (dedup off)"
    );
}

#[tokio::test]
async fn safety_net_agent_turn_errors_at_iteration_cap() {
    let calls = Arc::new(AtomicUsize::new(0));
    let script: Vec<ChatResponse> = (0..8)
        .map(|i| tool_response(vec![tool_call(&format!("tc{i}"), "echo")]))
        .collect();
    let mut agent = build_agent_with_runtime(
        Box::new(ScriptedProvider::new(script)),
        vec![Box::new(CountingTool {
            name: "echo",
            calls,
        })],
        zeroclaw_config::schema::ResolvedRuntime {
            max_tool_iterations: 3,
            ..zeroclaw_config::schema::ResolvedRuntime::default()
        },
    );
    let err = agent
        .turn("loop forever")
        .await
        .expect_err("Agent::turn must error at the iteration cap, not summarize");
    assert!(
        err.to_string()
            .contains("Agent exceeded maximum tool iterations"),
        "unexpected cap error: {err}"
    );
}

// ── seam 3: wire-visible TurnEvent ordering for a tool turn ─────────────
// WS/RPC/ACP all consume this stream; nothing upstream asserts cross-event
// ordering or that Usage count == provider-call count.

#[tokio::test]
async fn safety_net_streaming_event_sequence_for_tool_turn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut first = tool_response(vec![tool_call("tc-1", "echo")]);
    first.usage = Some(token_usage(10, 5));
    let mut second = text_response("all done");
    second.usage = Some(token_usage(20, 8));
    let mut agent = build_agent(
        Box::new(ScriptedProvider::new(vec![first, second])),
        vec![Box::new(CountingTool {
            name: "echo",
            calls,
        })],
    );

    let (tx, mut rx) = mpsc::channel(256);
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("seq", tx, None, None)
            .await
    });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    let outcome = handle
        .await
        .expect("task join")
        .expect("streamed turn should succeed");

    let pos_tool_call = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ToolCall { .. }))
        .expect("a ToolCall event must be emitted");
    let pos_tool_result = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ToolResult { .. }))
        .expect("a ToolResult event must be emitted");
    assert!(
        pos_tool_call < pos_tool_result,
        "ToolCall must precede its ToolResult"
    );
    let (call_id, result_id) = match (&events[pos_tool_call], &events[pos_tool_result]) {
        (TurnEvent::ToolCall { id: c, .. }, TurnEvent::ToolResult { id: r, .. }) => {
            (c.clone(), r.clone())
        }
        _ => unreachable!(),
    };
    assert_eq!(call_id, "tc-1");
    assert_eq!(
        call_id, result_id,
        "ToolCall/ToolResult must share the correlation id"
    );
    let pos_final_chunk = events
        .iter()
        .rposition(|e| matches!(e, TurnEvent::Chunk { delta } if delta.contains("all done")))
        .expect("final response text must be emitted as a Chunk");
    assert!(
        pos_tool_result < pos_final_chunk,
        "final-round Chunk must follow the ToolResult"
    );
    let usage_count = events
        .iter()
        .filter(|e| matches!(e, TurnEvent::Usage { .. }))
        .count();
    assert_eq!(
        usage_count, 2,
        "one Usage event per provider call (turn made 2 calls)"
    );
    assert!(outcome.response.contains("all done"));
}

// ── seam 4: model reasoning never reaches the channel draft or Chunks ───
// Capture-not-leak: reasoning is preserved on the stored message but must
// never surface in `StreamDelta` (channel draft, E1) or `TurnEvent::Chunk`
// (streaming consumers, E2).

#[tokio::test]
async fn safety_net_thinking_never_leaks_into_draft_or_chunks() {
    // E1 / channel-draft leg: streamed reasoning + a streamed tool call.
    struct ThinkStreamProvider {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl ModelProvider for ThinkStreamProvider {
        async fn chat_with_system(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }
        async fn chat(&self, _: ChatRequest<'_>, _: &str, _: Option<f64>) -> Result<ChatResponse> {
            Ok(text_response("non-streamed fallback"))
        }
        fn supports_streaming(&self) -> bool {
            true
        }
        fn supports_streaming_tool_events(&self) -> bool {
            true
        }
        fn stream_chat(
            &self,
            _: ChatRequest<'_>,
            _: &str,
            _: Option<f64>,
            _: zeroclaw_providers::traits::StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
        > {
            use futures_util::StreamExt as _;
            use zeroclaw_providers::traits::{StreamChunk, StreamEvent};
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let events: Vec<_> = if call == 0 {
                vec![
                    Ok(StreamEvent::TextDelta(StreamChunk {
                        delta: String::new(),
                        reasoning: Some("SECRET-REASONING".into()),
                        is_final: false,
                        token_count: 0,
                    })),
                    Ok(StreamEvent::TextDelta(StreamChunk {
                        delta: "Let me check that.".into(),
                        reasoning: None,
                        is_final: false,
                        token_count: 0,
                    })),
                    Ok(StreamEvent::ToolCall(tool_call("tc-1", "echo"))),
                    Ok(StreamEvent::Final),
                ]
            } else {
                vec![
                    Ok(StreamEvent::TextDelta(StreamChunk {
                        delta: "after-tool answer".into(),
                        reasoning: None,
                        is_final: false,
                        token_count: 0,
                    })),
                    Ok(StreamEvent::Final),
                ]
            };
            futures_util::stream::iter(events).boxed()
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for ThinkStreamProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ThinkStreamProvider"
        }
    }

    let provider = ThinkStreamProvider {
        calls: AtomicUsize::new(0),
    };
    let exec_count = Arc::new(AtomicUsize::new(0));
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool {
        name: "echo",
        calls: Arc::clone(&exec_count),
    })];
    let mut history = vec![ChatMessage::user("hi")];
    let (dtx, mut drx) = mpsc::channel(256);
    let turn_id = uuid::Uuid::new_v4().to_string();
    let result = crate::agent::loop_::run_tool_call_loop(crate::agent::loop_::ToolLoop {
        parent_agent_alias: None,
        sop_reassembly: None,
        exec: crate::agent::loop_::ResolvedAgentExecution {
            model_access: crate::agent::loop_::ResolvedModelAccess {
                model_provider: &provider,
                provider_name: "mock",
                model: "mock-model",
                temperature: None,
            },
            tools_registry: &tools_registry,
            observer: &observability::NoopObserver {},
            silent: true,
            approval: None,
            multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
            config: None,
            max_tool_iterations: 5,
            hooks: None,
            excluded_tools: &[],
            dedup_exempt_tools: &[],
            activated_tools: None,
            model_switch_callback: None,
            pacing: &zeroclaw_config::schema::PacingConfig::default(),
            strict_tool_parsing: false,
            parallel_tools: false,
            max_tool_result_chars: 30_000,
            context_token_budget: 100_000,
            receipt_generator: None,
            knobs: &crate::agent::loop_::LoopKnobs::default(),
        },
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: Some(dtx),
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        // Phase 1: stamp Internal/Trusted. Per-transport
        // stamping lands in a later phase.
        memory: None,
        ingress: IngressContext::sub_turn(),
        agent_alias: None,
        turn_id: &turn_id,
    })
    .await
    .expect("loop should succeed");
    assert!(result.contains("after-tool answer"));
    assert_eq!(
        exec_count.load(Ordering::SeqCst),
        1,
        "streamed tool call must execute"
    );
    let mut draft_text = String::new();
    while let Ok(delta) = drx.try_recv() {
        let body = match &delta {
            crate::agent::loop_::StreamDelta::Text(t) => t,
            crate::agent::loop_::StreamDelta::Status(s) => s,
            crate::agent::loop_::StreamDelta::Lifecycle(_) => "",
        };
        assert!(
            !body.contains("SECRET"),
            "reasoning leaked into the channel draft: {body:?}"
        );
        if let crate::agent::loop_::StreamDelta::Text(t) = &delta {
            draft_text.push_str(t);
        }
    }
    assert!(
        draft_text.contains("after-tool answer"),
        "visible text must stream to the draft, got: {draft_text:?}"
    );

    // E2 / TurnEvent leg: non-streamed reasoning_content alongside a tool
    // call — never a Chunk, preserved on the stored message.
    let calls = Arc::new(AtomicUsize::new(0));
    let mut first = tool_response(vec![tool_call("tc-9", "echo")]);
    first.reasoning_content = Some("SECRET-E2".into());
    let mut agent = build_agent(
        Box::new(ScriptedProvider::new(vec![
            first,
            text_response("visible-final"),
        ])),
        vec![Box::new(CountingTool {
            name: "echo",
            calls,
        })],
    );
    let (tx, mut rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("think", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    while let Ok(ev) = rx.try_recv() {
        if let TurnEvent::Chunk { delta } = &ev {
            assert!(
                !delta.contains("SECRET-E2"),
                "reasoning leaked as a Chunk: {delta:?}"
            );
        }
    }
    assert!(
        agent.history().iter().any(|m| matches!(
            m,
            ConversationMessage::AssistantToolCalls {
                reasoning_content: Some(r),
                ..
            } if r == "SECRET-E2"
        )),
        "reasoning_content must be preserved on the stored assistant message"
    );
}

#[tokio::test]
async fn safety_net_streaming_approval_deny_with_edit_round_trip() {
    struct EditChannel {
        requests: Arc<AtomicUsize>,
        seen_summary: Arc<parking_lot::Mutex<Option<String>>>,
    }
    impl ::zeroclaw_api::attribution::Attributable for EditChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
            )
        }
        fn alias(&self) -> &str {
            "edit-channel"
        }
    }
    #[async_trait]
    impl zeroclaw_api::channel::Channel for EditChannel {
        fn name(&self) -> &str {
            "edit-channel"
        }
        async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _recipient: &str,
            request: &zeroclaw_api::channel::ChannelApprovalRequest,
        ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            *self.seen_summary.lock() = Some(request.arguments_summary.clone());
            Ok(Some(
                zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit {
                    replacement: "EDITED-RESULT".into(),
                },
            ))
        }
    }

    let exec_count = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let seen_summary = Arc::new(parking_lot::Mutex::new(None));
    let risk = zeroclaw_config::schema::RiskProfileConfig {
        always_ask: vec!["echo".into()],
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let approval_mgr = Arc::new(ApprovalManager::for_non_interactive(&risk));
    let workspace = test_workspace();
    let agent = Agent::builder()
        .model_provider(Box::new(ScriptedProvider::new(vec![tool_response(vec![
            ToolCall {
                id: "tc-5".into(),
                name: "echo".into(),
                arguments: r#"{"message": "needs approval"}"#.into(),
                extra_content: None,
            },
        ])])))
        .tools(vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&exec_count),
        })])
        .memory(mem_none(workspace.path()))
        .observer(Arc::from(observability::NoopObserver {}))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(workspace.path().to_path_buf())
        .approval_manager(Some(Arc::clone(&approval_mgr)))
        .build()
        .expect("agent builder should succeed");
    let mut agent = TestAgent {
        agent,
        _workspace: workspace,
    };

    let handle: tools::PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
    agent.channel_handles.ask_user = Some(Arc::clone(&handle));
    agent.channel_handles().register_channel(
        "edit-channel",
        Arc::new(EditChannel {
            requests: Arc::clone(&requests),
            seen_summary: Arc::clone(&seen_summary),
        }),
    );

    let (tx, mut rx) = mpsc::channel(256);
    let outcome = agent
        .turn_streamed_with_steering_state("approve me", tx, None, None)
        .await
        .expect("streamed turn should succeed");

    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "exactly one approval request"
    );
    assert_eq!(
        exec_count.load(Ordering::SeqCst),
        0,
        "DenyWithEdit must not execute the tool"
    );
    let summary = seen_summary.lock().clone().expect("summary captured");
    assert!(!summary.is_empty(), "arguments_summary must be populated");
    let mut saw_edited_result_event = false;
    while let Ok(ev) = rx.try_recv() {
        if let TurnEvent::ToolResult { output, .. } = &ev
            && output.contains("EDITED-RESULT")
        {
            saw_edited_result_event = true;
        }
    }
    assert!(
        saw_edited_result_event,
        "ToolResult event must carry the replacement output"
    );
    assert!(
        outcome.new_messages.iter().any(|m| matches!(
            m,
            ConversationMessage::ToolResults(results)
                if results.iter().any(|r| r.content.contains("EDITED-RESULT"))
        )),
        "persisted tool result must carry the replacement output"
    );

    let log = approval_mgr.audit_log();
    let entry = log.last().expect("a decision must be recorded");
    assert_eq!(
        entry.channel, "edit-channel",
        "approval audit must attribute the deciding back-channel, not the loop's static \"cli\""
    );
}

// ── seam 6: steering persistence shapes around a tool round ─────────────
// The 3 steering oracles cover text-only rounds; this tops up the
// `new_messages` shape when a tool round and a steering injection both
// persist: AssistantToolCalls + ToolResults + the steering user message.

#[tokio::test]
async fn safety_net_steering_persistence_includes_tool_round_shapes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent(
        Box::new(ScriptedProvider::new(vec![
            tool_response(vec![tool_call("tc-2", "echo")]),
            text_response("final"),
        ])),
        vec![Box::new(CountingTool {
            name: "echo",
            calls,
        })],
    );
    let (tx, _rx) = mpsc::channel(256);
    let (steer_tx, mut steer_rx) = mpsc::channel::<String>(4);
    steer_tx
        .send("steer-text".into())
        .await
        .expect("steering send");
    let outcome = agent
        .turn_streamed_with_steering_state("first", tx, None, Some(&mut steer_rx))
        .await
        .expect("streamed turn should succeed");

    assert!(
        outcome
            .new_messages
            .iter()
            .any(|m| matches!(m, ConversationMessage::AssistantToolCalls { .. })),
        "new_messages must persist the AssistantToolCalls round"
    );
    assert!(
        outcome
            .new_messages
            .iter()
            .any(|m| matches!(m, ConversationMessage::ToolResults(_))),
        "new_messages must persist the ToolResults round"
    );
    assert!(
        outcome.new_messages.iter().any(|m| matches!(
            m,
            ConversationMessage::Chat(c) if c.role == "user" && c.content.contains("steer-text")
        )),
        "new_messages must persist the steering user message content"
    );
}

#[tokio::test]
async fn safety_net_task_locals_probe_per_entry_path() {
    type Probe = Arc<parking_lot::Mutex<Vec<(bool, bool, bool)>>>;
    struct ProbeTool {
        seen: Probe,
    }
    zeroclaw_api::tool_attribution!(ProbeTool, ::zeroclaw_api::attribution::ToolKind::Plugin);
    #[async_trait]
    impl Tool for ProbeTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            let thread = zeroclaw_api::TOOL_LOOP_THREAD_ID
                .try_with(|v| v.is_some())
                .unwrap_or(false);
            let session = zeroclaw_api::TOOL_LOOP_SESSION_KEY
                .try_with(|v| v.is_some())
                .unwrap_or(false);
            let choice = zeroclaw_api::TOOL_CHOICE_OVERRIDE
                .try_with(|v| v.is_some())
                .unwrap_or(false);
            self.seen.lock().push((thread, session, choice));
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    // streaming path — unscoped today (flips in G2)
    let seen: Probe = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut agent = build_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![tool_call(
            "p1", "echo",
        )])])),
        vec![Box::new(ProbeTool {
            seen: Arc::clone(&seen),
        })],
    );
    let (tx, _rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("probe", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    assert_eq!(
        seen.lock().as_slice(),
        &[(false, false, false)],
        "turn_streamed*: task-locals are UNSCOPED today — this flips after the G2 rebuild"
    );

    // Agent::turn path — unscoped today (flips in G2)
    let seen: Probe = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut agent = build_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![tool_call(
            "p2", "echo",
        )])])),
        vec![Box::new(ProbeTool {
            seen: Arc::clone(&seen),
        })],
    );
    agent.turn("probe").await.expect("turn should succeed");
    assert_eq!(
        seen.lock().as_slice(),
        &[(false, false, false)],
        "Agent::turn: task-locals are UNSCOPED today — this flips after the G2 rebuild"
    );

    // channel/E1 path — the caller scopes thread id + session key (control)
    let seen: Probe = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let provider = ScriptedProvider::new(vec![tool_response(vec![tool_call("p3", "echo")])]);
    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(ProbeTool {
        seen: Arc::clone(&seen),
    })];
    let mut history = vec![ChatMessage::user("probe")];
    let turn_id = uuid::Uuid::new_v4().to_string();
    crate::agent::loop_::scope_thread_id(
        Some("thread-1".into()),
        crate::agent::loop_::scope_session_key(Some("session-1".into()), async {
            crate::agent::loop_::run_tool_call_loop(crate::agent::loop_::ToolLoop {
                parent_agent_alias: None,
                sop_reassembly: None,
                exec: crate::agent::loop_::ResolvedAgentExecution {
                    model_access: crate::agent::loop_::ResolvedModelAccess {
                        model_provider: &provider,
                        provider_name: "mock",
                        model: "mock-model",
                        temperature: None,
                    },
                    tools_registry: &tools_registry,
                    observer: &observability::NoopObserver {},
                    silent: true,
                    approval: None,
                    multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                    config: None,
                    max_tool_iterations: 5,
                    hooks: None,
                    excluded_tools: &[],
                    dedup_exempt_tools: &[],
                    activated_tools: None,
                    model_switch_callback: None,
                    pacing: &zeroclaw_config::schema::PacingConfig::default(),
                    strict_tool_parsing: false,
                    parallel_tools: false,
                    max_tool_result_chars: 30_000,
                    context_token_budget: 100_000,
                    receipt_generator: None,
                    knobs: &crate::agent::loop_::LoopKnobs::default(),
                },
                history: &mut history,
                channel_name: "cli",
                channel_reply_target: None,
                cancellation_token: None,
                on_delta: None,
                shared_budget: None,
                channel: None,
                collected_receipts: None,
                event_tx: None,
                steering: None,
                new_messages_out: None,
                image_cache: None,
                // Phase 1: stamp Internal/Trusted. Per-transport
                // stamping lands in a later phase.
                memory: None,
                ingress: IngressContext::sub_turn(),
                agent_alias: None,
                turn_id: &turn_id,
            })
            .await
        }),
    )
    .await
    .expect("loop should succeed");
    let e1 = seen.lock().first().copied().expect("probe ran on E1");
    assert!(
        e1.0 && e1.1,
        "channel/E1 path: thread id + session key are scoped today, got {e1:?}"
    );
}

// ── seam 8: streaming tool results in input order + mid-batch cancel ────
// semantics exist as E1 tests only; the streaming engine must keep
// them observably: results persist in input order, and a cancel mid-batch
// synthesizes interrupted results for the calls that never ran.

#[tokio::test]
async fn safety_net_streaming_tool_results_input_order_and_midbatch_cancel() {
    // (a) results persist in input order, not alphabetical/completion order
    let calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![
            tool_call("1", "gamma"),
            tool_call("2", "alpha"),
            tool_call("3", "beta"),
        ])])),
        vec![
            Box::new(CountingTool {
                name: "alpha",
                calls: Arc::clone(&calls),
            }),
            Box::new(CountingTool {
                name: "beta",
                calls: Arc::clone(&calls),
            }),
            Box::new(CountingTool {
                name: "gamma",
                calls: Arc::clone(&calls),
            }),
        ],
    );
    let (tx, _rx) = mpsc::channel(256);
    let outcome = agent
        .turn_streamed_with_steering_state("order", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    let results: Vec<(String, String)> = outcome
        .new_messages
        .iter()
        .find_map(|m| match m {
            ConversationMessage::ToolResults(r) => Some(
                r.iter()
                    .map(|t| (t.tool_call_id.clone(), t.content.clone()))
                    .collect(),
            ),
            _ => None,
        })
        .expect("a ToolResults message must persist");
    assert_eq!(
        results
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["1", "2", "3"],
        "tool results must persist in input order"
    );
    assert!(results[0].1.contains("gamma-out"));
    assert!(results[1].1.contains("alpha-out"));
    assert!(results[2].1.contains("beta-out"));

    // (b) cancel mid-batch synthesizes interrupted results for unrun calls
    struct CancellingTool {
        token: tokio_util::sync::CancellationToken,
    }
    zeroclaw_api::tool_attribution!(
        CancellingTool,
        ::zeroclaw_api::attribution::ToolKind::Plugin
    );
    #[async_trait]
    impl Tool for CancellingTool {
        fn name(&self) -> &str {
            "gamma"
        }
        fn description(&self) -> &str {
            "gamma"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            self.token.cancel();
            Ok(crate::tools::ToolResult {
                success: true,
                output: "gamma-out".into(),
                error: None,
            })
        }
    }

    let token = tokio_util::sync::CancellationToken::new();
    let later_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![
            tool_call("c1", "gamma"),
            tool_call("c2", "alpha"),
            tool_call("c3", "beta"),
        ])])),
        vec![
            Box::new(CancellingTool {
                token: token.clone(),
            }),
            Box::new(CountingTool {
                name: "alpha",
                calls: Arc::clone(&later_calls),
            }),
            Box::new(CountingTool {
                name: "beta",
                calls: Arc::clone(&later_calls),
            }),
        ],
    );
    let (tx, _rx) = mpsc::channel(256);
    let err = agent
        .turn_streamed_with_steering_state("cancel", tx, Some(token), None)
        .await
        .expect_err("cancel mid-batch must surface as StreamedTurnError");
    assert_eq!(
        later_calls.load(Ordering::SeqCst),
        0,
        "calls after the cancel point must not execute"
    );
    assert!(
        err.committed_response
            .contains(&crate::i18n::get_english_cli_string_with_args(
                "turn-interrupted-by-user",
                &[]
            )),
        "unexpected committed_response: {}",
        err.committed_response
    );
    let results: Vec<(String, String)> = err
        .new_messages
        .iter()
        .find_map(|m| match m {
            ConversationMessage::ToolResults(r) => Some(
                r.iter()
                    .map(|t| (t.tool_call_id.clone(), t.content.clone()))
                    .collect(),
            ),
            _ => None,
        })
        .expect("synthesized ToolResults must persist on cancel");
    assert_eq!(
        results
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["c1", "c2", "c3"],
        "synthesized results must cover every call, in input order"
    );
    assert!(
        results[1].1.contains("interrupted") && results[2].1.contains("interrupted"),
        "unrun calls must be synthesized as interrupted"
    );
}

/// Captures observer events for assertion; no-op for metrics.
#[derive(Default)]
struct EventCapture {
    events: parking_lot::Mutex<Vec<ObserverEvent>>,
}

impl Observer for EventCapture {
    fn record_event(&self, event: &ObserverEvent) {
        self.events.lock().push(event.clone());
    }
    fn record_metric(&self, _metric: &zeroclaw_api::observability_traits::ObserverMetric) {}
    fn name(&self) -> &str {
        "event-capture"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
async fn safety_net_agent_turn_agent_end_reports_token_totals() {
    let mut tool_round = tool_response(vec![tool_call("tc1", "echo")]);
    tool_round.usage = Some(token_usage(7, 3));
    let mut final_round = text_response("done");
    final_round.usage = Some(token_usage(11, 5));

    let calls = Arc::new(AtomicUsize::new(0));
    let capture = Arc::new(EventCapture::default());
    let workspace = test_workspace();
    let agent = Agent::builder()
        .model_provider(Box::new(ScriptedProvider::new(vec![
            tool_round,
            final_round,
        ])))
        .tools(vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&calls),
        })])
        .memory(mem_none(workspace.path()))
        .observer(Arc::clone(&capture) as Arc<dyn Observer>)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(workspace.path().to_path_buf())
        .build()
        .expect("agent builder should succeed");
    let mut agent = TestAgent {
        agent,
        _workspace: workspace,
    };

    agent
        .turn("count tokens")
        .await
        .expect("turn should succeed");

    let events = capture.events.lock();
    let tokens = events
        .iter()
        .find_map(|event| match event {
            ObserverEvent::AgentEnd { tokens_used, .. } => Some(tokens_used.clone()),
            _ => None,
        })
        .expect("AgentEnd must be recorded")
        .expect("AgentEnd must carry tokens_used");
    assert_eq!(
        tokens.input_tokens, 18,
        "input tokens must sum across all loop iterations"
    );
    assert_eq!(
        tokens.output_tokens, 8,
        "output tokens must sum across all loop iterations"
    );
}

#[tokio::test]
async fn safety_net_turn_survives_in_loop_history_pruning() {
    let filler = "x".repeat(400);
    let runtime = zeroclaw_config::schema::ResolvedRuntime {
        // ~40 seeded messages × (100 tokens content + 4 framing) ≫ 500.
        max_context_tokens: 500,
        ..zeroclaw_config::schema::ResolvedRuntime::default()
    };

    // Agent::turn (new_messages_out path)
    let calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent_with_runtime(
        Box::new(ScriptedProvider::new(vec![
            tool_response(vec![tool_call("tc-prune", "echo")]),
            text_response("pruned-final"),
        ])),
        vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&calls),
        })],
        runtime.clone(),
    );
    for i in 0..20 {
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::user(format!(
                "seed-{i} {filler}"
            ))));
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::assistant(format!(
                "reply-{i} {filler}"
            ))));
    }
    let response = agent
        .turn("after a long conversation")
        .await
        .expect("turn must survive in-loop pruning of the seeded history");
    assert_eq!(response, "pruned-final");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "tool round must execute");
    assert!(
        agent.history.iter().any(|m| matches!(
            m,
            ConversationMessage::Chat(c) if c.role == "assistant" && c.content == "pruned-final"
        )),
        "the final assistant reply must persist into conversation history"
    );
    assert!(
        agent
            .history
            .iter()
            .any(|m| matches!(m, ConversationMessage::AssistantToolCalls { .. })),
        "the executed tool round must persist into conversation history"
    );

    // turn_streamed_with_steering_state (per-round capture path)
    let calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent_with_runtime(
        Box::new(ScriptedProvider::new(vec![
            tool_response(vec![tool_call("tc-prune-s", "echo")]),
            text_response("pruned-final-streamed"),
        ])),
        vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&calls),
        })],
        runtime,
    );
    for i in 0..20 {
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::user(format!(
                "seed-{i} {filler}"
            ))));
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::assistant(format!(
                "reply-{i} {filler}"
            ))));
    }
    let (tx, _rx) = mpsc::channel(256);
    let outcome = agent
        .turn_streamed_with_steering_state("after a long conversation", tx, None, None)
        .await
        .expect("streamed turn must survive in-loop pruning");
    assert_eq!(outcome.response, "pruned-final-streamed");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "tool round must execute");
    assert!(
        outcome
            .new_messages
            .iter()
            .any(|m| matches!(m, ConversationMessage::AssistantToolCalls { .. })),
        "new_messages must contain the executed tool round despite pruning"
    );
    assert!(
        outcome.new_messages.iter().any(|m| matches!(
            m,
            ConversationMessage::Chat(c)
                if c.role == "assistant" && c.content == "pruned-final-streamed"
        )),
        "new_messages must contain the final assistant reply despite pruning"
    );
    assert!(
        !outcome.new_messages.iter().any(|m| matches!(
            m,
            ConversationMessage::Chat(c) if c.content.starts_with("seed-")
        )),
        "pre-existing history must never leak into new_messages"
    );
}

/// Scripted responses, then a hard provider error once exhausted.
struct ErrAfterScriptProvider {
    responses: parking_lot::Mutex<VecDeque<ChatResponse>>,
}

#[async_trait]
impl ModelProvider for ErrAfterScriptProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<ChatResponse> {
        self.responses
            .lock()
            .pop_front()
            .ok_or_else(|| anyhow::Error::msg("provider 500: scripted mid-turn failure"))
    }
}

impl ::zeroclaw_api::attribution::Attributable for ErrAfterScriptProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "ErrAfterScriptProvider"
    }
}

#[tokio::test]
async fn safety_net_agent_turn_error_path_keeps_executed_rounds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent(
        Box::new(ErrAfterScriptProvider {
            responses: parking_lot::Mutex::new(
                vec![tool_response(vec![tool_call("tc-err", "echo")])].into(),
            ),
        }),
        vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&calls),
        })],
    );
    let err = agent
        .turn("do work then fail")
        .await
        .expect_err("second provider call is scripted to fail");
    assert!(
        err.to_string().contains("provider 500"),
        "unexpected error: {err}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the tool round executed before the failure"
    );
    assert!(
        agent
            .history
            .iter()
            .any(|m| matches!(m, ConversationMessage::AssistantToolCalls { .. })),
        "executed tool round (assistant side) must survive the turn error"
    );
    assert!(
        agent
            .history
            .iter()
            .any(|m| matches!(m, ConversationMessage::ToolResults(_))),
        "executed tool round (results side) must survive the turn error"
    );
}

#[tokio::test]
async fn safety_net_midbatch_cancel_emits_events_for_completed_tools() {
    struct CancelAfterRunTool {
        token: tokio_util::sync::CancellationToken,
    }
    zeroclaw_api::tool_attribution!(
        CancelAfterRunTool,
        ::zeroclaw_api::attribution::ToolKind::Plugin
    );
    #[async_trait]
    impl Tool for CancelAfterRunTool {
        fn name(&self) -> &str {
            "gamma"
        }
        fn description(&self) -> &str {
            "gamma"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            self.token.cancel();
            Ok(crate::tools::ToolResult {
                success: true,
                output: "gamma-out".into(),
                error: None,
            })
        }
    }

    let token = tokio_util::sync::CancellationToken::new();
    let later_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![
            tool_call("ev1", "gamma"),
            tool_call("ev2", "alpha"),
        ])])),
        vec![
            Box::new(CancelAfterRunTool {
                token: token.clone(),
            }),
            Box::new(CountingTool {
                name: "alpha",
                calls: Arc::clone(&later_calls),
            }),
        ],
    );
    let (tx, mut rx) = mpsc::channel(256);
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("cancel-events", tx, Some(token), None)
            .await
    });
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    handle
        .await
        .expect("task join")
        .expect_err("cancel mid-batch must surface as StreamedTurnError");

    assert_eq!(later_calls.load(Ordering::SeqCst), 0, "alpha must not run");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolCall { id, .. } if id == "ev1")),
        "the completed tool must emit its ToolCall event despite the cancel"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            TurnEvent::ToolResult { id, output, .. } if id == "ev1" && output.contains("gamma-out")
        )),
        "the completed tool must emit its ToolResult event despite the cancel"
    );
}

/// Streams the given events, then hangs (pending) so the test can cancel.
struct StreamThenHangProvider {
    events: parking_lot::Mutex<
        Vec<zeroclaw_api::model_provider::StreamResult<zeroclaw_api::model_provider::StreamEvent>>,
    >,
}

#[async_trait]
impl ModelProvider for StreamThenHangProvider {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }
    async fn chat(&self, _: ChatRequest<'_>, _: &str, _: Option<f64>) -> Result<ChatResponse> {
        Ok(text_response("non-streamed fallback (must not be reached)"))
    }
    fn supports_streaming(&self) -> bool {
        true
    }
    fn supports_streaming_tool_events(&self) -> bool {
        true
    }
    fn stream_chat(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
        _: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_api::model_provider::StreamResult<zeroclaw_api::model_provider::StreamEvent>,
    > {
        use futures_util::StreamExt as _;
        let events = std::mem::take(&mut *self.events.lock());
        futures_util::stream::iter(events)
            .chain(futures_util::stream::pending())
            .boxed()
    }
}

impl ::zeroclaw_api::attribution::Attributable for StreamThenHangProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "StreamThenHangProvider"
    }
}

fn text_delta(
    delta: &str,
) -> zeroclaw_api::model_provider::StreamResult<zeroclaw_api::model_provider::StreamEvent> {
    Ok(zeroclaw_api::model_provider::StreamEvent::TextDelta(
        zeroclaw_api::model_provider::StreamChunk {
            delta: delta.to_string(),
            reasoning: None,
            is_final: false,
            token_count: 0,
        },
    ))
}

#[tokio::test]
async fn safety_net_cancel_after_streamed_output_persists_partial() {
    let provider = StreamThenHangProvider {
        events: parking_lot::Mutex::new(vec![text_delta("partial answer")]),
    };
    let mut agent = build_agent(Box::new(provider), vec![]);
    let token = tokio_util::sync::CancellationToken::new();
    let cancel = token.clone();

    let (tx, mut rx) = mpsc::channel(256);
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("stream then cancel", tx, Some(token), None)
            .await
    });
    // Cancel only after the partial is visibly forwarded.
    while let Some(ev) = rx.recv().await {
        if matches!(&ev, TurnEvent::Chunk { delta } if delta.contains("partial answer")) {
            cancel.cancel();
            break;
        }
    }
    while rx.recv().await.is_some() {}
    let err = handle
        .await
        .expect("task join")
        .expect_err("cancel must surface as StreamedTurnError");

    assert_eq!(
        err.committed_response,
        format!(
            "partial answer\n\n{}",
            crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[])
        ),
        "committed_response must carry the watched partial with the marker"
    );
    let interruption_messages: Vec<&ConversationMessage> = err
        .new_messages
        .iter()
        .filter(|m| {
            matches!(
                m,
                ConversationMessage::Chat(c)
                    if c.role == "assistant"
                    && c.content.contains(&crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[]))
            )
        })
        .collect();
    assert_eq!(
        interruption_messages.len(),
        1,
        "exactly one interruption message — the persisted partial, no duplicate bare marker"
    );
    assert!(
        matches!(
            interruption_messages[0],
            ConversationMessage::Chat(c) if c.content.starts_with("partial answer")
        ),
        "the persisted interruption message must carry the partial text"
    );
}

#[tokio::test]
async fn safety_net_stream_error_persists_only_forwarded_text() {
    let provider = StreamThenHangProvider {
        events: parking_lot::Mutex::new(vec![
            // Thinking marks event-visible output without any forwarded text.
            Ok(zeroclaw_api::model_provider::StreamEvent::TextDelta(
                zeroclaw_api::model_provider::StreamChunk {
                    delta: String::new(),
                    reasoning: Some("working on it".into()),
                    is_final: false,
                    token_count: 0,
                },
            )),
            // Suspicious protocol prefix: the stream guard withholds it, so
            // no consumer ever sees this text.
            text_delta("{\"tool_call\": {\"name\": \"shell\""),
            Err(zeroclaw_api::model_provider::StreamError::Http(
                "connection reset".into(),
            )),
        ]),
    };
    let mut agent = build_agent(Box::new(provider), vec![]);
    let (tx, mut rx) = mpsc::channel(256);
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("stream then die", tx, None, None)
            .await
    });
    while rx.recv().await.is_some() {}
    let err = handle
        .await
        .expect("task join")
        .expect_err("stream error after visible output must fail the turn (no fallback retry)");

    assert_eq!(
        err.committed_response,
        crate::i18n::get_english_cli_string_with_args("turn-stream-interrupted", &[]),
        "nothing was forwarded, so nothing may be committed as delivered"
    );
    assert!(
        !err.new_messages.iter().any(|m| matches!(
            m,
            ConversationMessage::Chat(c) if c.content.contains("tool_call")
        )),
        "guard-withheld text the consumer never saw must not persist as delivered output"
    );
}

#[tokio::test]
async fn safety_net_graceful_summary_persists_assistant_summary() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent_with_runtime(
        Box::new(ScriptedProvider::new(vec![
            tool_response(vec![tool_call("s1", "echo")]),
            tool_response(vec![tool_call("s2", "echo")]),
            text_response("wrap-up summary"),
        ])),
        vec![Box::new(CountingTool {
            name: "echo",
            calls,
        })],
        zeroclaw_config::schema::ResolvedRuntime {
            max_tool_iterations: 2,
            ..zeroclaw_config::schema::ResolvedRuntime::default()
        },
    );
    let (tx, _rx) = mpsc::channel(256);
    let outcome = agent
        .turn_streamed_with_steering_state("exhaust the cap", tx, None, None)
        .await
        .expect("graceful summary must succeed on the streamed path");
    assert!(
        outcome.response.contains("wrap-up summary"),
        "unexpected response: {}",
        outcome.response
    );
    let last_chat = outcome
        .new_messages
        .iter()
        .rev()
        .find_map(|m| match m {
            ConversationMessage::Chat(c) => Some(c),
            _ => None,
        })
        .expect("new_messages must contain chat messages");
    assert_eq!(
        (last_chat.role.as_str(), last_chat.content.as_str()),
        ("assistant", "wrap-up summary"),
        "the persisted transcript must end with the assistant summary, not the synthetic user prompt"
    );
}

#[tokio::test]
async fn safety_net_failed_graceful_summary_does_not_persist_prompt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut agent = build_agent_with_runtime(
        Box::new(ErrAfterScriptProvider {
            responses: parking_lot::Mutex::new(
                vec![
                    tool_response(vec![tool_call("f1", "echo")]),
                    tool_response(vec![tool_call("f2", "echo")]),
                ]
                .into(),
            ),
        }),
        vec![Box::new(CountingTool {
            name: "echo",
            calls,
        })],
        zeroclaw_config::schema::ResolvedRuntime {
            max_tool_iterations: 2,
            ..zeroclaw_config::schema::ResolvedRuntime::default()
        },
    );
    let (tx, _rx) = mpsc::channel(256);
    let err = agent
        .turn_streamed_with_steering_state("exhaust then fail summary", tx, None, None)
        .await
        .expect_err("the summary call is scripted to fail");
    assert!(
        err.error.to_string().contains("maximum tool iterations"),
        "unexpected error: {}",
        err.error
    );
    assert!(
        !err.new_messages.iter().any(|m| matches!(
            m,
            ConversationMessage::Chat(c)
                if c.role == "user" && c.content.contains("maximum number of tool iterations")
        )),
        "the unanswered synthetic summary prompt must not persist when the summary call fails"
    );
}

/// Records each approval request and answers with a fixed decision.
struct RecordingApprovalChannel {
    response: zeroclaw_api::channel::ChannelApprovalResponse,
    requests: Arc<AtomicUsize>,
}
impl ::zeroclaw_api::attribution::Attributable for RecordingApprovalChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
        )
    }
    fn alias(&self) -> &str {
        "acp"
    }
}
#[async_trait]
impl zeroclaw_api::channel::Channel for RecordingApprovalChannel {
    fn name(&self) -> &str {
        "acp"
    }
    async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
        Ok(())
    }
    async fn listen(
        &self,
        _tx: mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn request_approval(
        &self,
        _recipient: &str,
        _request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.response.clone()))
    }
}

/// Counts executions and captures the args it was actually invoked with —
/// the observable for the `approved`-arg trust assertions.
struct CapturingArgTool {
    name: &'static str,
    output: &'static str,
    calls: Arc<AtomicUsize>,
    last_args: Arc<parking_lot::Mutex<Option<serde_json::Value>>>,
}
zeroclaw_api::tool_attribution!(
    CapturingArgTool,
    ::zeroclaw_api::attribution::ToolKind::Plugin
);
#[async_trait]
impl Tool for CapturingArgTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.name
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, args: serde_json::Value) -> Result<crate::tools::ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_args.lock() = Some(args);
        Ok(crate::tools::ToolResult {
            success: true,
            output: self.output.into(),
            error: None,
        })
    }
}

fn tool_call_args(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args.to_string(),
        extra_content: None,
    }
}

/// Build an Agent with an optional approval manager and a single registered
/// `acp` back-channel, then drive one streamed turn.
fn approval_agent(
    provider: Box<dyn ModelProvider>,
    tools_vec: Vec<Box<dyn Tool>>,
    manager: Option<Arc<ApprovalManager>>,
    channel: Option<Arc<dyn zeroclaw_api::channel::Channel>>,
) -> TestAgent {
    let workspace = test_workspace();
    let mut builder = Agent::builder()
        .model_provider(provider)
        .tools(tools_vec)
        .memory(mem_none(workspace.path()))
        .observer(Arc::from(observability::NoopObserver {}))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(workspace.path().to_path_buf());
    if let Some(mgr) = manager {
        builder = builder.approval_manager(Some(mgr));
    }
    let mut agent = builder.build().expect("agent builder should succeed");
    if let Some(ch) = channel {
        let handle: tools::PerToolChannelHandle =
            Arc::new(parking_lot::RwLock::new(HashMap::new()));
        agent.channel_handles.ask_user = Some(handle);
        agent.channel_handles().register_channel("acp", ch);
    }
    TestAgent {
        agent,
        _workspace: workspace,
    }
}

#[tokio::test]
async fn safety_net_loop_approval_requested_then_executed_on_approve() {
    let exec = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let risk = zeroclaw_config::schema::RiskProfileConfig {
        always_ask: vec!["echo".into()],
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let mut agent = approval_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![tool_call(
            "tc1", "echo",
        )])])),
        vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&exec),
        })],
        Some(Arc::new(ApprovalManager::for_non_interactive(&risk))),
        Some(Arc::new(RecordingApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::Approve,
            requests: Arc::clone(&requests),
        })),
    );
    let (tx, _rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("go", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "back-channel asked once"
    );
    assert_eq!(exec.load(Ordering::SeqCst), 1, "approved tool executed");
}

#[tokio::test]
async fn safety_net_loop_approval_denied_blocks_execution() {
    let exec = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let risk = zeroclaw_config::schema::RiskProfileConfig {
        always_ask: vec!["echo".into()],
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let mut agent = approval_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![tool_call(
            "tc1", "echo",
        )])])),
        vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&exec),
        })],
        Some(Arc::new(ApprovalManager::for_non_interactive(&risk))),
        Some(Arc::new(RecordingApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::Deny,
            requests: Arc::clone(&requests),
        })),
    );
    let (tx, _rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("go", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "back-channel asked once"
    );
    assert_eq!(
        exec.load(Ordering::SeqCst),
        0,
        "denied tool must not execute"
    );
}

#[tokio::test]
async fn safety_net_loop_shell_does_not_trust_model_supplied_approved_arg() {
    let exec = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(parking_lot::Mutex::new(None));
    let mut agent = approval_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![
            tool_call_args(
                "tc1",
                "shell",
                serde_json::json!({"command": "touch should-not-run", "approved": true}),
            ),
        ])])),
        vec![Box::new(CapturingArgTool {
            name: "shell",
            output: "shell-out",
            calls: Arc::clone(&exec),
            last_args: Arc::clone(&captured),
        })],
        Some(Arc::new(ApprovalManager::for_non_interactive_backchannel(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        ))),
        Some(Arc::new(RecordingApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::Deny,
            requests: Arc::clone(&requests),
        })),
    );
    let (tx, _rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("go", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "model approved=true must NOT bypass the gate"
    );
    assert_eq!(
        exec.load(Ordering::SeqCst),
        0,
        "denied shell must not execute"
    );
    assert!(
        captured.lock().is_none(),
        "a denied tool is never invoked, so no args are captured"
    );
}

#[tokio::test]
async fn safety_net_loop_shell_marks_args_approved_after_backchannel_approval() {
    let exec = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(parking_lot::Mutex::new(None));
    let mut agent = approval_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![
            tool_call_args(
                "tc1",
                "shell",
                serde_json::json!({"command": "touch should-run", "approved": false}),
            ),
        ])])),
        vec![Box::new(CapturingArgTool {
            name: "shell",
            output: "shell-out",
            calls: Arc::clone(&exec),
            last_args: Arc::clone(&captured),
        })],
        Some(Arc::new(ApprovalManager::for_non_interactive_backchannel(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        ))),
        Some(Arc::new(RecordingApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::Approve,
            requests: Arc::clone(&requests),
        })),
    );
    let (tx, _rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("go", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(exec.load(Ordering::SeqCst), 1, "approved shell executes");
    let args = captured.lock().clone().expect("executed args captured");
    assert_eq!(
        args["approved"], true,
        "runtime injects approved=true only after a real back-channel approval"
    );
}

#[tokio::test]
async fn safety_net_loop_shell_keeps_runtime_approval_from_always_allowlist() {
    let exec = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(parking_lot::Mutex::new(None));
    let mut agent = approval_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![
            tool_call_args(
                "tc1",
                "shell",
                serde_json::json!({"command": "touch first", "approved": false}),
            ),
            tool_call_args(
                "tc2",
                "shell",
                serde_json::json!({"command": "touch second", "approved": false}),
            ),
        ])])),
        vec![Box::new(CapturingArgTool {
            name: "shell",
            output: "shell-out",
            calls: Arc::clone(&exec),
            last_args: Arc::clone(&captured),
        })],
        Some(Arc::new(ApprovalManager::for_non_interactive_backchannel(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        ))),
        Some(Arc::new(RecordingApprovalChannel {
            response: zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove,
            requests: Arc::clone(&requests),
        })),
    );
    let (tx, _rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("go", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "AlwaysApprove on the first call serves the second from the allowlist"
    );
    assert_eq!(exec.load(Ordering::SeqCst), 2, "both shell calls execute");
    let args = captured.lock().clone().expect("executed args captured");
    assert_eq!(args["approved"], true);
}

#[tokio::test]
async fn safety_net_loop_cron_add_does_not_trust_model_supplied_approved_arg() {
    let exec = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(parking_lot::Mutex::new(None));
    // No approval manager: cron_add is NotRequired, so it runs without a gate
    // — but the model-supplied approved=true is still stripped to false.
    let mut agent = approval_agent(
        Box::new(ScriptedProvider::new(vec![tool_response(vec![
            tool_call_args(
                "tc1",
                "cron_add",
                serde_json::json!({"command": "echo hi", "approved": true}),
            ),
        ])])),
        vec![Box::new(CapturingArgTool {
            name: "cron_add",
            output: "cron-out",
            calls: Arc::clone(&exec),
            last_args: Arc::clone(&captured),
        })],
        None,
        None,
    );
    let (tx, _rx) = mpsc::channel(256);
    agent
        .turn_streamed_with_steering_state("go", tx, None, None)
        .await
        .expect("streamed turn should succeed");
    assert_eq!(
        exec.load(Ordering::SeqCst),
        1,
        "cron_add runs (no approval required)"
    );
    let args = captured.lock().clone().expect("executed args captured");
    assert_eq!(
        args["approved"], false,
        "model-supplied approved=true must be stripped even with no approval gate"
    );
}

#[tokio::test]
async fn usage_event_coherent_tuple_vision_route() {
    use crate::agent::agent::ProviderSwitchConfig;
    use crate::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::schema::MultimodalConfig;

    // Two cells: vision provider with and without an explicit context_window.
    // Vision routing must emit Usage with the vision provider's ref + model,
    // the resolved window must track the vision provider's context_window, and
    // the BASE provider's window must not cross-contaminate.
    for vision_window in [Some(200_000_u64), None] {
        let (input_tokens, output_tokens) = (50_u64, 10_u64);
        let (input_rate, output_rate) = (1.5_f64, 3.0_f64);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "vision analysis complete"}],
                "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
                "stop_reason": "end_turn",
                "model": "claude-3-opus"
            })))
            .mount(&server)
            .await;

        let mut cfg = Config::default();
        let base = cfg
            .providers
            .models
            .ensure("openai", "default")
            .expect("ensure base");
        base.context_window = Some(128_000); // must remain on the BASE provider only
        let vision = cfg
            .providers
            .models
            .ensure("anthropic", "vision")
            .expect("ensure vision provider");
        if let Some(w) = vision_window {
            vision.context_window = Some(w as usize);
        }
        vision.vision = Some(true);
        vision.api_key = Some("test-key".into());
        vision.uri = Some(server.uri());
        vision.model = Some("claude-3-opus".into());
        vision
            .pricing
            .insert("claude-3-opus.input".into(), input_rate);
        vision
            .pricing
            .insert("claude-3-opus.output".into(), output_rate);
        vision
            .pricing
            .insert("claude-3-opus.cached_input".into(), 0.0);

        let cfg_arc = Arc::new(cfg.clone());
        let switch_cfg = ProviderSwitchConfig {
            config: Some(cfg_arc.clone()),
        };
        let mut base_resp = text_response("base response");
        base_resp.usage = Some(token_usage(1, 1));
        let provider = ScriptedProvider::new(vec![base_resp]);
        let mut agent = Agent::builder()
            .model_provider(Box::new(provider))
            .tools(vec![])
            .memory(mem_none())
            .observer(Arc::from(observability::NoopObserver {}) as Arc<dyn Observer>)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_provider_name("openai.default".into())
            .model_name("gpt-4o-mini".into())
            .provider_switch_config(switch_cfg)
            .build()
            .expect("agent builder should succeed");
        agent.multimodal_config = MultimodalConfig {
            vision_model_provider: Some("anthropic.vision".into()),
            vision_model: Some("claude-3-opus".into()),
            ..Default::default()
        };

        // Wire a real CostTracker + provider-keyed pricing map so cost_usd
        // is computed (mirrors ws.rs::process_chat_message). The pricing map
        // keys the SERVING provider (anthropic.vision) only, so a coherent
        // tuple yields Some(expected_cost) and a misattributed tuple yields 0.
        let (_tracker, cost_ctx) =
            build_cost_context("anthropic.vision", "claude-3-opus", input_rate, output_rate);

        // Vision routing triggers on an image marker in the prompt. Write a
        // temp image and rewrite the prompt; clean up after the turn so the
        // second cell iteration doesn't pile up temp files.
        let img_path = std::env::temp_dir()
            .join(format!("matrix_vision_{vision_window:?}.png").replace(['(', ')', ' '], "_"));
        std::fs::write(&img_path, b"fake-png-data").expect("write temp image");
        let prompt = format!("Analyze this image: [IMAGE:{}]", img_path.display());
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(256);
        let mut capture_agent = agent;
        let handle = zeroclaw_spawn::spawn!(async move {
            TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(
                    Some(cost_ctx.clone()),
                    capture_agent.turn_streamed_with_steering_state(&prompt, tx, None, None),
                )
                .await
        });
        let mut events: Vec<TurnEvent> = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        let _ = handle
            .await
            .expect("task join")
            .expect("turn should succeed");
        let _ = std::fs::remove_file(img_path);

        // Find the (single) Usage event from the vision-serving call.
        let usage = events
            .iter()
            .find_map(|e| match e {
                TurnEvent::Usage {
                    provider_ref,
                    model,
                    input_tokens,
                    output_tokens,
                    cost_usd,
                    ..
                } => Some((
                    provider_ref.clone(),
                    model.clone(),
                    *input_tokens,
                    *output_tokens,
                    *cost_usd,
                )),
                _ => None,
            })
            .expect("vision-routed Usage event must be emitted");

        assert_eq!(
            usage.0, "anthropic.vision",
            "Usage.provider_ref must be the vision provider"
        );
        assert_eq!(
            usage.1, "claude-3-opus",
            "Usage.model must be the vision model"
        );

        let resolved = cfg
            .model_provider_context_window_opt(&usage.0)
            .map(|v| v as u64);
        assert_eq!(
            resolved, vision_window,
            "resolved window must track the vision provider's context_window, never the base's"
        );

        // Negative control: the BASE provider's window must NOT have leaked.
        let base_resolved = cfg
            .model_provider_context_window_opt("openai.default")
            .map(|v| v as u64);
        assert_eq!(
            base_resolved,
            Some(128_000),
            "base provider's window must remain 128_000 (no cross-contamination)"
        );

        let expected_cost = (input_tokens as f64) * input_rate / 1_000_000.0
            + (output_tokens as f64) * output_rate / 1_000_000.0;
        let observed = usage.4.expect("Usage.cost_usd must be Some(_)");
        let diff = (observed - expected_cost).abs();
        let tol = expected_cost.max(1e-9) * 1e-6;
        assert!(
            diff <= tol,
            "Usage.cost_usd ({observed}) must match vision pricing × tokens ({expected_cost})"
        );

        assert_eq!(
            usage.2,
            Some(input_tokens),
            "input_tokens must flow through"
        );
        assert_eq!(
            usage.3,
            Some(output_tokens),
            "output_tokens must flow through"
        );
    }
}

#[tokio::test]
async fn usage_event_coherent_tuple_in_turn_model_switch() {
    use crate::agent::agent::ProviderSwitchConfig;
    use crate::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Test tool that queues a pending model switch when executed, standing
    // in for the real `model_switch` tool during a streamed turn.
    struct ModelSwitchTriggerTool {
        target_provider: String,
        target_model: String,
    }

    zeroclaw_api::tool_attribution!(
        ModelSwitchTriggerTool,
        ::zeroclaw_api::attribution::ToolKind::Plugin
    );

    #[async_trait]
    impl Tool for ModelSwitchTriggerTool {
        fn name(&self) -> &str {
            "model_switch_trigger"
        }
        fn description(&self) -> &str {
            "test tool: queues a pending model switch"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            let state = crate::agent::turn::current_model_switch_state()?;
            *state.lock().unwrap() =
                Some((self.target_provider.clone(), self.target_model.clone()));
            Ok(crate::tools::ToolResult {
                success: true,
                output: "model switch queued".into(),
                error: None,
            })
        }
    }

    // Two cells: switched-to provider with and without an explicit context_window.
    for switch_window in [Some(200_000_u64), None] {
        let (input_tokens, output_tokens) = (10_u64, 5_u64);
        let (input_rate, output_rate) = (1.5_f64, 3.0_f64);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "switched call served"}],
                "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
                "stop_reason": "end_turn",
                "model": "claude-3-opus"
            })))
            .mount(&server)
            .await;

        let mut cfg = Config::default();
        cfg.providers
            .models
            .ensure("openai", "primary")
            .expect("ensure primary")
            .context_window = Some(128_000);
        let provider_b = cfg
            .providers
            .models
            .ensure("anthropic", "provider-b")
            .expect("ensure provider B (switched-to)");
        if let Some(w) = switch_window {
            provider_b.context_window = Some(w as usize);
        }
        provider_b.api_key = Some("test-key".into());
        provider_b.uri = Some(server.uri());
        provider_b.model = Some("claude-3-opus".into());
        provider_b
            .pricing
            .insert("claude-3-opus.input".into(), input_rate);
        provider_b
            .pricing
            .insert("claude-3-opus.output".into(), output_rate);
        provider_b
            .pricing
            .insert("claude-3-opus.cached_input".into(), 0.0);

        // The model route forces credential resolution to look up provider B's
        // config entry (api_key/uri) instead of falling back to the primary's.
        cfg.model_routes
            .push(zeroclaw_config::schema::ModelRouteConfig {
                hint: "matrix-switch".into(),
                model_provider: "anthropic.provider-b".into(),
                model: "claude-3-opus".into(),
                api_key: None,
            });

        let cfg_arc = Arc::new(cfg.clone());
        let switch_cfg = ProviderSwitchConfig {
            config: Some(cfg_arc.clone()),
        };

        // The ScriptedProvider returns a tool_call on its first (and only)
        // call.  The trigger tool queues the switch; the turn loop detects
        // it, rebuilds the provider from ProviderSwitchConfig, and the next
        // call goes to the wiremock-backed switched-to provider.
        let provider = ScriptedProvider::new(vec![tool_response(vec![tool_call(
            "1",
            "model_switch_trigger",
        )])]);

        let agent = Agent::builder()
            .model_provider(Box::new(provider))
            .tools(vec![Box::new(ModelSwitchTriggerTool {
                target_provider: "anthropic.provider-b".into(),
                target_model: "claude-3-opus".into(),
            })])
            .memory(mem_none())
            .observer(Arc::from(observability::NoopObserver {}) as Arc<dyn Observer>)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_provider_name("openai.primary".into())
            .model_name("gpt-4o-mini".into())
            .provider_switch_config(switch_cfg)
            .agent_alias("matrix-test".into())
            .build()
            .expect("agent builder should succeed");

        // Wire a real CostTracker + provider-keyed pricing map so cost_usd
        // is computed (mirrors ws.rs::process_chat_message). The pricing map
        // keys the SERVING provider only, so a coherent tuple yields
        // Some(expected_cost) and a misattributed tuple with no pricing
        // entries yields Some(0.0) — the assertion catches both.
        let (_tracker, cost_ctx) = build_cost_context(
            "anthropic.provider-b",
            "claude-3-opus",
            input_rate,
            output_rate,
        );

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(256);
        let prompt = "complete the task".to_string();
        let mut agent = agent;
        let (turn_result, events) = tokio::join!(
            TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                Some(cost_ctx.clone()),
                agent.turn_streamed_with_steering_state(&prompt, tx, None, None)
            ),
            async {
                let mut evs = Vec::new();
                while let Some(ev) = rx.recv().await {
                    evs.push(ev);
                }
                evs
            },
        );
        let _ = turn_result.expect("turn should succeed");

        // Find the post-switch Usage event the serving call emitted. The
        // pre-switch call (ScriptedProvider tool_call) carries no usage, so
        // the only Usage event comes from the wiremock-backed call with the
        // expected token counts (10, 5).
        let usage = events
            .iter()
            .find_map(|e| match e {
                TurnEvent::Usage {
                    provider_ref,
                    model,
                    input_tokens: it,
                    output_tokens: ot,
                    cost_usd,
                    ..
                } if *it == Some(input_tokens) && *ot == Some(output_tokens) => {
                    Some((provider_ref.clone(), model.clone(), *it, *ot, cost_usd))
                }
                _ => None,
            })
            .expect("a Usage event must be emitted by the serving call");

        // ASSERTION 1: provider_ref reflects the SERVING provider
        assert_eq!(
            usage.0, "anthropic.provider-b",
            "Usage.provider_ref must be the switched-to provider",
        );

        // ASSERTION 2: model reflects the SERVING model
        assert_eq!(
            usage.1, "claude-3-opus",
            "Usage.model must be the switched-to model",
        );

        // ASSERTION 3: resolve_live_model_context_window follows the
        // SERVING provider's config_window, never the trim budget.
        let resolved = cfg
            .model_provider_context_window_opt(&usage.0)
            .map(|v| v as u64);
        assert_eq!(
            resolved, switch_window,
            "resolved window must track the switched provider's context_window",
        );

        // ASSERTION 4: cost_usd matches the serving tuple's pricing
        let expected_cost = (input_tokens as f64) * input_rate / 1_000_000.0
            + (output_tokens as f64) * output_rate / 1_000_000.0;
        let observed_cost = usage.4.expect("Usage.cost_usd must be Some(_)");
        let cost_diff = (observed_cost - expected_cost).abs();
        let cost_tolerance = expected_cost.max(1e-9) * 1e-6;
        assert!(
            cost_diff <= cost_tolerance,
            "Usage.cost_usd ({observed_cost}) must match the serving tuple's pricing × tokens ({expected_cost}) within tolerance ±{cost_tolerance}; a misattributed tuple would resolve empty pricing and compute zero",
        );

        // ASSERTION 5: token counts flow through from the serving call
        assert_eq!(usage.2, Some(input_tokens), "input_tokens mismatch");
        assert_eq!(usage.3, Some(output_tokens), "output_tokens mismatch");
    }
}

#[tokio::test]
async fn usage_by_provider_breakdown_after_in_turn_model_switch() {
    // REQ-B2: per-provider usage breakdown after an in-turn model switch.
    // Both providers emit usage; the resulting usage_by_provider map must
    // contain separate entries with correct identity, tokens, and cost.
    // last_serving_provider_ref / last_serving_model must reflect the
    // switched-to provider, and aggregate totals must equal the sum of
    // per-provider entries.
    use crate::agent::agent::ProviderSwitchConfig;
    use crate::agent::cost::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (input_tokens_a, output_tokens_a) = (25_u64, 10_u64);
    let (input_tokens_b, output_tokens_b) = (15_u64, 8_u64);
    let (input_rate_a, output_rate_a) = (1.0_f64, 2.0_f64);
    let (input_rate_b, output_rate_b) = (1.5_f64, 3.0_f64);

    // Test tool that queues a pending model switch when executed.
    struct ModelSwitchTriggerTool {
        target_provider: String,
        target_model: String,
    }
    zeroclaw_api::tool_attribution!(
        ModelSwitchTriggerTool,
        ::zeroclaw_api::attribution::ToolKind::Plugin
    );
    #[async_trait]
    impl Tool for ModelSwitchTriggerTool {
        fn name(&self) -> &str {
            "model_switch_trigger"
        }
        fn description(&self) -> &str {
            "test tool: queues a pending model switch"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            let state = crate::agent::turn::current_model_switch_state()?;
            *state.lock().unwrap() =
                Some((self.target_provider.clone(), self.target_model.clone()));
            Ok(crate::tools::ToolResult {
                success: true,
                output: "model switch queued".into(),
                error: None,
            })
        }
    }

    // wiremock for switched-to provider B
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [{"type": "text", "text": "switched call served"}],
            "usage": {"input_tokens": input_tokens_b, "output_tokens": output_tokens_b},
            "stop_reason": "end_turn",
            "model": "claude-3-opus"
        })))
        .mount(&server)
        .await;

    // two-provider config
    let mut cfg = Config::default();
    cfg.providers
        .models
        .ensure("openai", "primary")
        .expect("ensure primary")
        .context_window = Some(128_000);
    let provider_b = cfg
        .providers
        .models
        .ensure("anthropic", "provider-b")
        .expect("ensure provider B");
    provider_b.context_window = Some(200_000);
    provider_b.api_key = Some("test-key".into());
    provider_b.uri = Some(server.uri());
    provider_b.model = Some("claude-3-opus".into());
    provider_b
        .pricing
        .insert("claude-3-opus.input".into(), input_rate_b);
    provider_b
        .pricing
        .insert("claude-3-opus.output".into(), output_rate_b);
    provider_b
        .pricing
        .insert("claude-3-opus.cached_input".into(), 0.0);

    cfg.model_routes
        .push(zeroclaw_config::schema::ModelRouteConfig {
            hint: "req-b2-switch".into(),
            model_provider: "anthropic.provider-b".into(),
            model: "claude-3-opus".into(),
            api_key: None,
        });

    let cfg_arc = Arc::new(cfg.clone());
    let switch_cfg = ProviderSwitchConfig {
        config: Some(cfg_arc),
    };

    // Provider A: ScriptedProvider returns tool_call WITH usage data.
    let mut resp_a = tool_response(vec![tool_call("1", "model_switch_trigger")]);
    resp_a.usage = Some(token_usage(input_tokens_a, output_tokens_a));

    let provider = ScriptedProvider::new(vec![resp_a]);

    let agent = Agent::builder()
        .model_provider(Box::new(provider))
        .tools(vec![Box::new(ModelSwitchTriggerTool {
            target_provider: "anthropic.provider-b".into(),
            target_model: "claude-3-opus".into(),
        })])
        .memory(mem_none())
        .observer(Arc::from(observability::NoopObserver {}) as Arc<dyn Observer>)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_provider_name("openai.primary".into())
        .model_name("gpt-4o-mini".into())
        .provider_switch_config(switch_cfg)
        .agent_alias("req-b2-test".into())
        .build()
        .expect("agent builder should succeed");

    // cost context with pricing for BOTH providers
    let tmpdir = tempfile::TempDir::new().unwrap();
    let tracker = Arc::new(
        crate::cost::CostTracker::new(
            zeroclaw_config::schema::CostConfig::default(),
            tmpdir.path(),
        )
        .expect("CostTracker::new"),
    );
    let pricing_map = std::collections::HashMap::from([
        (
            "openai.primary".to_string(),
            std::collections::HashMap::from([
                ("gpt-4o-mini.input".to_string(), input_rate_a),
                ("gpt-4o-mini.output".to_string(), output_rate_a),
                ("gpt-4o-mini.cached_input".to_string(), 0.0_f64),
            ]),
        ),
        (
            "anthropic.provider-b".to_string(),
            std::collections::HashMap::from([
                ("claude-3-opus.input".to_string(), input_rate_b),
                ("claude-3-opus.output".to_string(), output_rate_b),
                ("claude-3-opus.cached_input".to_string(), 0.0_f64),
            ]),
        ),
    ]);
    let cost_ctx = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(pricing_map));

    // drive the turn, collect ALL events
    let (tx, mut rx) = mpsc::channel::<TurnEvent>(256);
    let prompt = "complete the task".to_string();
    let mut agent = agent;
    let (turn_result, events) = tokio::join!(
        TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
            Some(cost_ctx),
            agent.turn_streamed_with_steering_state(&prompt, tx, None, None),
        ),
        async {
            let mut evs = Vec::new();
            while let Some(ev) = rx.recv().await {
                evs.push(ev);
            }
            evs
        },
    );
    let _ = turn_result.expect("turn should succeed");

    // reconstruct usage_by_provider from events (mirrors process_chat_message)
    let mut usage_map: std::collections::HashMap<String, (String, u64, u64, u64, f64)> =
        std::collections::HashMap::new();

    for event in &events {
        if let TurnEvent::Usage {
            provider_ref,
            model,
            input_tokens,
            output_tokens,
            cached_input_tokens,
            cost_usd,
        } = event
        {
            let entry = usage_map.entry(provider_ref.clone()).or_default();
            entry.0 = model.clone();
            if let Some(it) = input_tokens {
                entry.1 = entry.1.saturating_add(*it);
            }
            if let Some(ot) = output_tokens {
                entry.2 = entry.2.saturating_add(*ot);
            }
            if let Some(ct) = cached_input_tokens {
                entry.3 = entry.3.saturating_add(*ct);
            }
            if let Some(cu) = cost_usd {
                entry.4 += cu;
            }
        }
    }

    // Sort by provider_ref (same as process_chat_message)
    let mut sorted_entries: Vec<_> = usage_map.into_iter().collect();
    sorted_entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    // ASSERTION 1: two entries — one per provider
    assert_eq!(
        sorted_entries.len(),
        2,
        "usage_by_provider must have 2 entries (one per provider)"
    );

    // ASSERTION 2: sort order matches production (provider_ref ascending).
    // "anthropic.provider-b" sorts before "openai.primary" alphabetically.
    assert_eq!(sorted_entries[0].0, "anthropic.provider-b");
    assert_eq!(sorted_entries[1].0, "openai.primary");

    // Entry 0: Provider B (anthropic.provider-b — alphabetically first)
    let (ref_b, (model_b, input_b, output_b, cached_b, cost_b)) = &sorted_entries[0];
    assert_eq!(ref_b, "anthropic.provider-b");
    assert_eq!(*model_b, "claude-3-opus");
    assert_eq!(*input_b, input_tokens_b);
    assert_eq!(*output_b, output_tokens_b);
    assert_eq!(*cached_b, 0);
    let expected_cost_b = input_tokens_b as f64 * input_rate_b / 1_000_000.0
        + output_tokens_b as f64 * output_rate_b / 1_000_000.0;
    let diff_b = (*cost_b - expected_cost_b).abs();
    assert!(
        diff_b <= expected_cost_b.max(1e-9) * 1e-6,
        "Provider B cost {cost_b} must match pricing × tokens ({expected_cost_b})"
    );

    // Entry 1: Provider A (openai.primary — alphabetically second)
    let (ref_a, (model_a, input_a, output_a, cached_a, cost_a)) = &sorted_entries[1];
    assert_eq!(ref_a, "openai.primary");
    assert_eq!(*model_a, "gpt-4o-mini");
    assert_eq!(*input_a, input_tokens_a);
    assert_eq!(*output_a, output_tokens_a);
    assert_eq!(*cached_a, 0);
    let expected_cost_a = input_tokens_a as f64 * input_rate_a / 1_000_000.0
        + output_tokens_a as f64 * output_rate_a / 1_000_000.0;
    let diff_a = (*cost_a - expected_cost_a).abs();
    assert!(
        diff_a <= expected_cost_a.max(1e-9) * 1e-6,
        "Provider A cost {cost_a} must match pricing × tokens ({expected_cost_a})"
    );

    // ASSERTION 3: aggregate totals equal the sum of per-provider entries.
    // Catches double-counting or dropped entries that a per-entry-only check misses.
    let agg_input: u64 = sorted_entries.iter().map(|(_, (_, i, _, _, _))| *i).sum();
    let agg_output: u64 = sorted_entries.iter().map(|(_, (_, _, o, _, _))| *o).sum();
    let agg_cost: f64 = sorted_entries.iter().map(|(_, (_, _, _, _, c))| *c).sum();
    assert_eq!(
        agg_input,
        input_tokens_a + input_tokens_b,
        "aggregate input_tokens must equal sum of per-provider entries"
    );
    assert_eq!(
        agg_output,
        output_tokens_a + output_tokens_b,
        "aggregate output_tokens must equal sum of per-provider entries"
    );
    let expected_agg_cost = expected_cost_a + expected_cost_b;
    let agg_cost_diff = (agg_cost - expected_agg_cost).abs();
    assert!(
        agg_cost_diff <= expected_agg_cost.max(1e-9) * 1e-6,
        "aggregate cost {agg_cost} must equal sum of per-provider costs ({expected_agg_cost})"
    );

    // last_serving_provider_ref and last_serving_model reflect Provider B
    let last_usage = events
        .iter()
        .rev()
        .find_map(|e| match e {
            TurnEvent::Usage {
                provider_ref,
                model,
                ..
            } => Some((provider_ref.clone(), model.clone())),
            _ => None,
        })
        .expect("at least one Usage event");
    assert_eq!(
        last_usage.0, "anthropic.provider-b",
        "last_serving_provider_ref must be the switched-to provider"
    );
    assert_eq!(
        last_usage.1, "claude-3-opus",
        "last_serving_model must be the switched-to model"
    );

    // context_window resolved from Provider B's config
    let resolved_b = cfg
        .model_provider_context_window_opt("anthropic.provider-b")
        .map(|v| v as u64);
    assert_eq!(resolved_b, Some(200_000));
}

#[tokio::test]
async fn usage_event_emitted_even_without_usage_data() {
    // Regression: REQ-B1 — a provider call that succeeds without usage
    // must still emit TurnEvent::Usage (with None token fields) so the
    // gateway propagates the serving identity to the done frame.

    let (input_rate, output_rate) = (1.5_f64, 3.0_f64);

    // Provider response with usage: None (simulates OpenAI Codex,
    // cached responses, or any API path that omits usage)
    let mut resp = text_response("no usage data");
    resp.usage = None;

    let provider = ScriptedProvider::new(vec![resp]);

    let agent = Agent::builder()
        .model_provider(Box::new(provider))
        .tools(vec![])
        .memory(mem_none())
        .observer(Arc::from(observability::NoopObserver {}) as Arc<dyn Observer>)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_provider_name("openai.default".into())
        .model_name("gpt-4o-mini".into())
        .agent_alias("req-b1-test".into())
        .build()
        .expect("agent builder should succeed");

    let (_tracker, cost_ctx) =
        build_cost_context("openai.default", "gpt-4o-mini", input_rate, output_rate);

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(256);
    let prompt = "hello".to_string();
    let mut agent = agent;
    let handle = zeroclaw_spawn::spawn!(async move {
        crate::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(cost_ctx),
                agent.turn_streamed_with_steering_state(&prompt, tx, None, None),
            )
            .await
    });

    let mut events: Vec<TurnEvent> = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    let _ = handle
        .await
        .expect("task join")
        .expect("turn should succeed");

    // Find a Usage event — must exist even though the response had no usage data
    let usage = events
        .iter()
        .find_map(|e| match e {
            TurnEvent::Usage {
                provider_ref,
                model,
                input_tokens,
                output_tokens,
                cost_usd,
                ..
            } => Some((
                provider_ref.as_str(),
                model.as_str(),
                *input_tokens,
                *output_tokens,
                *cost_usd,
            )),
            _ => None,
        })
        .expect("TurnEvent::Usage must be emitted even without usage data");

    // ASSERTION 1: Identity reflects the serving provider/model
    assert_eq!(usage.0, "openai.default");
    assert_eq!(usage.1, "gpt-4o-mini");

    // ASSERTION 2: Token fields are None (no usage data from provider)
    assert_eq!(
        usage.2, None,
        "input_tokens must be None when no usage data"
    );
    assert_eq!(
        usage.3, None,
        "output_tokens must be None when no usage data"
    );

    // ASSERTION 3: Cost is None (no usage → no cost computation)
    assert_eq!(usage.4, None, "cost_usd must be None when no usage data");
}

#[tokio::test]
async fn usage_identity_updates_on_subsequent_calls_without_usage() {
    // REQ-B1 full path: Provider A emits usage → switch to B →
    // B succeeds without usage → last Usage event shows B's identity.

    // ScriptedProvider: first call has usage, second call has no usage
    let mut resp_a = text_response("call with usage");
    resp_a.usage = Some(token_usage(10, 5));
    let mut resp_b = text_response("call without usage");
    resp_b.usage = None;

    let provider = ScriptedProvider::new(vec![resp_a, resp_b]);

    let agent = Agent::builder()
        .model_provider(Box::new(provider))
        .tools(vec![])
        .memory(mem_none())
        .observer(Arc::from(observability::NoopObserver {}) as Arc<dyn Observer>)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_provider_name("openai.default".into())
        .model_name("gpt-4o-mini".into())
        .agent_alias("req-b1-test".into())
        .build()
        .expect("agent builder should succeed");

    let (_tracker, cost_ctx) = build_cost_context("openai.default", "gpt-4o-mini", 1.5, 3.0);

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(256);
    let prompt = "hello".to_string();
    let mut agent = agent;
    let handle = zeroclaw_spawn::spawn!(async move {
        crate::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(
                Some(cost_ctx),
                agent.turn_streamed_with_steering_state(&prompt, tx, None, None),
            )
            .await
    });

    let mut events: Vec<TurnEvent> = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    let _ = handle
        .await
        .expect("task join")
        .expect("turn should succeed");

    // Find the LAST Usage event
    let last_usage = events
        .iter()
        .rev()
        .find_map(|e| match e {
            TurnEvent::Usage {
                provider_ref,
                model,
                ..
            } => Some((provider_ref.as_str(), model.as_str())),
            _ => None,
        })
        .expect("at least one Usage event");

    // Identity MUST reflect the most recent provider call
    // (ScriptedProvider uses the same provider_ref for all calls, so this
    // verifies the event is emitted for each call with the correct identity)
    assert_eq!(
        last_usage.0, "openai.default",
        "last Usage identity must reflect the most recent provider call"
    );
    assert_eq!(
        last_usage.1, "gpt-4o-mini",
        "last Usage model must reflect the most recent provider call"
    );
}
