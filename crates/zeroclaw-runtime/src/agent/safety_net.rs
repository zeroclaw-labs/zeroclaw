//! #7415 safety net — pins the turn-engine behaviors the existing suite is
//! known NOT to cover (spec: the eight seams in the consolidation plan).
//!
//! These tests pass against the UNMODIFIED engines and must stay green
//! through every extraction commit. Expected-to-change flips (each must be
//! updated in the same commit that changes the behavior, never silently):
//!
//! - task-locals on the streaming/`Agent::turn` paths: unscoped → scoped
//!   (`safety_net_task_locals_probe_per_entry_path`)
//! - streaming max-iteration outcome: error → graceful summary
//!   (`Agent::turn` keeps the error — `safety_net_agent_turn_errors_at_iteration_cap`
//!   must NOT flip)
//!
//! Related oracles that live elsewhere and are never modified: the 44
//! `run_tool_call_loop_*` tests, `agent/tests.rs::turn_bails_out_at_max_iterations`,
//! and the 3 steering oracles in `agent.rs`.

use super::*;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use zeroclaw_api::model_provider::TokenUsage;
use zeroclaw_providers::{ChatResponse, ToolCall};

// ── shared fixtures ─────────────────────────────────────────────────────

fn mem_none() -> Arc<dyn Memory> {
    let cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    Arc::from(
        zeroclaw_memory::create_memory(&cfg, Path::new("/tmp"), None)
            .expect("memory creation should succeed"),
    )
}

fn text_response(text: &str) -> ChatResponse {
    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: "{}".into(),
        extra_content: None,
    }
}

fn tool_response(calls: Vec<ToolCall>) -> ChatResponse {
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

/// Returns scripted responses in order; "done" once the script is exhausted.
struct ScriptedProvider {
    responses: parking_lot::Mutex<VecDeque<ChatResponse>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
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
struct CountingTool {
    name: &'static str,
    calls: Arc<AtomicUsize>,
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
            output: format!("{}-out", self.name),
            error: None,
        })
    }
}

fn build_agent(provider: Box<dyn ModelProvider>, tools_vec: Vec<Box<dyn Tool>>) -> Agent {
    Agent::builder()
        .model_provider(provider)
        .tools(tools_vec)
        .memory(mem_none())
        .observer(Arc::from(observability::NoopObserver {}))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed")
}

fn build_agent_with_runtime(
    provider: Box<dyn ModelProvider>,
    tools_vec: Vec<Box<dyn Tool>>,
    resolved: zeroclaw_config::schema::ResolvedRuntime,
) -> Agent {
    Agent::builder()
        .model_provider(provider)
        .tools(tools_vec)
        .memory(mem_none())
        .observer(Arc::from(observability::NoopObserver {}))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .config(zeroclaw_config::schema::AliasedAgentConfig {
            resolved,
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        })
        .build()
        .expect("agent builder should succeed")
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

// ── seam 2: Agent::turn ERRORS at the iteration cap ─────────────────────
// Embedder control signal: routing through the loop's graceful summary
// would silently replace the error with text. Complements
// `agent/tests.rs::turn_bails_out_at_max_iterations`; this pins the exact
// message prefix. Must NOT flip in G2.

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
    let result = crate::agent::loop_::run_tool_call_loop(
        &provider,
        &mut history,
        &tools_registry,
        &observability::NoopObserver {},
        "mock",
        "mock-model",
        None,
        true,
        None,
        "cli",
        None,
        &zeroclaw_config::schema::MultimodalConfig::default(),
        5,
        None,
        Some(dtx),
        None,
        &[],
        &[],
        None,
        None,
        &zeroclaw_config::schema::PacingConfig::default(),
        false,
        false,
        30_000,
        100_000,
        None,
        None,
        None,
        None,
        None, // event_tx
        None, // steering
        None, // new_messages_out
        &crate::agent::loop_::LoopKnobs::default(),
    )
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

// ── seam 5: approval round-trip on the streaming path, incl. DenyWithEdit ─
// The correlation contract is pause/resume through a registered back-channel;
// DenyWithEdit must complete the call with the (sanitized) replacement as the
// tool output without executing the tool. Existing ACP tests cover
// Approve/Deny but not DenyWithEdit; the gateway `request_id` layer
// (ws_approval.rs) has no tests and sits above this seam.

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
    let mut agent = Agent::builder()
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
        .memory(mem_none())
        .observer(Arc::from(observability::NoopObserver {}))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .approval_manager(Some(Arc::new(ApprovalManager::for_non_interactive(&risk))))
        .build()
        .expect("agent builder should succeed");

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

// ── seam 7: task-local probe per entry path ─────────────────────────────
// Records, from INSIDE tool execution, whether TOOL_LOOP_THREAD_ID /
// TOOL_LOOP_SESSION_KEY / TOOL_CHOICE_OVERRIDE are scoped. Today: the
// channel/E1 path is scoped by its caller; the streaming and Agent::turn
// paths are NOT. The streaming/Agent expectation flips to scoped in G2
// (the eighth gap) — flip it in that commit, never silently.

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
    crate::agent::loop_::scope_thread_id(
        Some("thread-1".into()),
        crate::agent::loop_::scope_session_key(Some("session-1".into()), async {
            crate::agent::loop_::run_tool_call_loop(
                &provider,
                &mut history,
                &tools_registry,
                &observability::NoopObserver {},
                "mock",
                "mock-model",
                None,
                true,
                None,
                "cli",
                None,
                &zeroclaw_config::schema::MultimodalConfig::default(),
                5,
                None,
                None,
                None,
                &[],
                &[],
                None,
                None,
                &zeroclaw_config::schema::PacingConfig::default(),
                false,
                false,
                30_000,
                100_000,
                None,
                None,
                None,
                None,
                None, // event_tx
                None, // steering
                None, // new_messages_out
                &crate::agent::loop_::LoopKnobs::default(),
            )
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
// #1043 semantics exist as E1 tests only; the streaming engine must keep
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
        err.committed_response.contains("[interrupted by user]"),
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

// ── seam 9: AgentEnd carries token totals on Agent::turn ────────────────
// E3 summed per-response usage straight into its TurnGuard. After the C4
// consolidation the wrapper no longer sees per-call responses; totals flow
// through the usage-only cost-tracking context instead (plan flag §8.6).
// Pins: AgentEnd.tokens_used = usage summed across ALL loop iterations.

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
    let mut agent = Agent::builder()
        .model_provider(Box::new(ScriptedProvider::new(vec![
            tool_round,
            final_round,
        ])))
        .tools(vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&calls),
        })])
        .memory(mem_none())
        .observer(Arc::clone(&capture) as Arc<dyn Observer>)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed");

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

// ── seam 10: turn survives in-loop history pruning ──────────────────────
// The loop's preflight maintenance prunes `history` in place when the token
// estimate exceeds `max_context_tokens`. `new_messages_out` (Agent::turn)
// and the streamed wrapper's per-round capture must not be derived from
// pre-prune history indices: that panics (slice start past the shrunken
// length) or silently persists the wrong messages.

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

// ── seam 11: Agent::turn keeps executed rounds on a later-call error ────
// Tools that ran carry side effects. The pre-consolidation engine pushed
// each round into `self.history` as it happened, so rounds survived a
// later-iteration provider failure; losing them makes a retry re-run
// side-effecting work the model can no longer see.

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

// ── seam 12: completed tools still emit events/hooks on mid-batch cancel ─
// A tool that RAN before the user cancelled must emit its TurnEvent
// ToolCall/ToolResult pair (and fire after_tool_call) even though the
// cancellation surfaces right after — otherwise the live event stream and
// the persisted transcript permanently disagree about what executed. The
// old streamed engine emitted these live, per tool, before the cancel hit.

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

// ── seam 13: streamed-partial fidelity on interruption ──────────────────
// (a) A user cancel after visible streamed text persists the watched
//     partial with "[interrupted by user]" (the old streaming engine's
//     committed-partial-on-cancel), without a duplicate bare marker.
// (b) A stream error persists only text the consumer actually SAW
//     (forwarded chunks), never guard-withheld protocol fragments.

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
        err.committed_response, "partial answer\n\n[interrupted by user]",
        "committed_response must carry the watched partial with the marker"
    );
    let interruption_messages: Vec<&ConversationMessage> = err
        .new_messages
        .iter()
        .filter(|m| {
            matches!(
                m,
                ConversationMessage::Chat(c)
                    if c.role == "assistant" && c.content.contains("[interrupted by user]")
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
        err.committed_response, "[stream interrupted]",
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

// ── seam 14: the graceful max-iteration summary persists coherently ─────
// GracefulSummary pushes a synthetic "provide your best answer" user
// message and delivers the model's summary as the response. The summary
// must ALSO persist as the answering assistant message — otherwise
// persistent-history callers (streamed wrapper, new_messages consumers)
// store a transcript ending on an unanswered synthetic user prompt and the
// delivered summary is absent from the conversation.

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

/// Companion to seam 14: when the summary call itself FAILS, the synthetic
/// prompt must not persist either — a transcript ending on the unanswered
/// "provide your best answer" prompt is the incoherence under test, and the
/// failure branch must not reintroduce it.
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
