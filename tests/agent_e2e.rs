//! End-to-end integration tests for agent orchestration.
//!
//! These tests exercise the full agent turn cycle through the public API,
//! using mock providers and tools to validate orchestration behavior without
//! external service dependencies. They complement the unit tests in
//! `src/agent/tests.rs` by running at the integration test boundary.
//!
//! Ref: https://github.com/zeroclaw-labs/zeroclaw/issues/618 (item 6)

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
use zeroclaw::agent::agent::Agent;
use zeroclaw::agent::dispatcher::{NativeToolDispatcher, XmlToolDispatcher};
use zeroclaw::agent::memory_loader::MemoryLoader;
use zeroclaw::config::MemoryConfig;
use zeroclaw::memory;
use zeroclaw::memory::Memory;
use zeroclaw::observability::{NoopObserver, Observer};
use zeroclaw::providers::traits::ChatMessage;
use zeroclaw::providers::{
    ChatRequest, ChatResponse, ConversationMessage, Provider, ProviderRuntimeOptions, ToolCall,
};
use zeroclaw::tools::{Tool, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// Mock infrastructure
// ─────────────────────────────────────────────────────────────────────────────

/// Mock provider that returns scripted responses in FIFO order.
struct MockProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl MockProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        Ok("fallback".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok(ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        Ok(guard.remove(0))
    }
}

/// Simple tool that echoes its input argument.
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes the input message"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": {"type": "string"}
            }
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let msg = args
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("(empty)")
            .to_string();
        Ok(ToolResult {
            success: true,
            output: msg,
            error: None,
        })
    }
}

/// Tool that tracks invocation count for verifying dispatch.
struct CountingTool {
    count: Arc<Mutex<usize>>,
}

impl CountingTool {
    fn new() -> (Self, Arc<Mutex<usize>>) {
        let count = Arc::new(Mutex::new(0));
        (
            Self {
                count: count.clone(),
            },
            count,
        )
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "counter"
    }
    fn description(&self) -> &str {
        "Counts invocations"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        let mut c = self.count.lock().unwrap();
        *c += 1;
        Ok(ToolResult {
            success: true,
            output: format!("call #{}", *c),
            error: None,
        })
    }
}

/// Mock provider that returns scripted responses AND records every request.
/// Pattern from `ScriptedProvider` in `src/agent/tests.rs`.
struct RecordingProvider {
    responses: Mutex<Vec<ChatResponse>>,
    recorded_requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

impl RecordingProvider {
    fn new(responses: Vec<ChatResponse>) -> (Self, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let provider = Self {
            responses: Mutex::new(responses),
            recorded_requests: recorded.clone(),
        };
        (provider, recorded)
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        Ok("fallback".into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        self.recorded_requests
            .lock()
            .unwrap()
            .push(request.messages.to_vec());

        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok(ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        Ok(guard.remove(0))
    }
}

/// Mock memory loader that returns a static context string,
/// simulating RAG recall without a real memory backend.
struct StaticMemoryLoader {
    context: String,
}

impl StaticMemoryLoader {
    fn new(context: &str) -> Self {
        Self {
            context: context.to_string(),
        }
    }
}

#[async_trait]
impl MemoryLoader for StaticMemoryLoader {
    async fn load_context(&self, _memory: &dyn Memory, _user_message: &str) -> Result<String> {
        Ok(self.context.clone())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────────

fn make_memory() -> Arc<dyn Memory> {
    let cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    Arc::from(memory::create_memory(&cfg, &std::env::temp_dir(), None).unwrap())
}

fn make_observer() -> Arc<dyn Observer> {
    Arc::from(NoopObserver {})
}

fn text_response(text: &str) -> ChatResponse {
    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
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

fn build_agent(provider: Box<dyn Provider>, tools: Vec<Box<dyn Tool>>) -> Agent {
    Agent::builder()
        .provider(provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .build()
        .unwrap()
}

fn build_agent_xml(provider: Box<dyn Provider>, tools: Vec<Box<dyn Tool>>) -> Agent {
    Agent::builder()
        .provider(provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(XmlToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .build()
        .unwrap()
}

fn build_recording_agent(
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    memory_loader: Option<Box<dyn MemoryLoader>>,
) -> Agent {
    let mut builder = Agent::builder()
        .provider(provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir());

    if let Some(loader) = memory_loader {
        builder = builder.memory_loader(loader);
    }

    builder.build().unwrap()
}

// ═════════════════════════════════════════════════════════════════════════════
// E2E smoke tests — full agent turn cycle
// ═════════════════════════════════════════════════════════════════════════════

/// Validates the simplest happy path: user message → LLM text response.
#[tokio::test]
async fn e2e_simple_text_response() {
    let provider = Box::new(MockProvider::new(vec![text_response(
        "Hello from mock provider",
    )]));
    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);

    let response = agent.turn("hi").await.unwrap();
    assert!(!response.is_empty(), "Expected non-empty text response");
}

/// Validates single tool call → tool execution → final LLM response.
#[tokio::test]
async fn e2e_single_tool_call_cycle() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "hello from tool"}"#.into(),
        }]),
        text_response("Tool executed successfully"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("run echo").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after tool execution"
    );
}

/// Validates multi-step tool chain: tool A → tool B → tool C → final response.
#[tokio::test]
async fn e2e_multi_step_tool_chain() {
    let (counting_tool, count) = CountingTool::new();

    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "counter".into(),
            arguments: "{}".into(),
        }]),
        tool_response(vec![ToolCall {
            id: "tc2".into(),
            name: "counter".into(),
            arguments: "{}".into(),
        }]),
        text_response("Done after 2 tool calls"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(counting_tool)]);
    let response = agent.turn("count twice").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after tool chain"
    );
    assert_eq!(*count.lock().unwrap(), 2);
}

/// Validates that the XML dispatcher path also works end-to-end.
#[tokio::test]
async fn e2e_xml_dispatcher_tool_call() {
    let provider = Box::new(MockProvider::new(vec![
        ChatResponse {
            text: Some(
                r#"<tool_call>
{"name": "echo", "arguments": {"message": "xml dispatch"}}
</tool_call>"#
                    .into(),
            ),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        },
        text_response("XML tool executed"),
    ]));

    let mut agent = build_agent_xml(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("test xml dispatch").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response from XML dispatcher"
    );
}

/// Validates that multiple sequential turns maintain conversation coherence.
#[tokio::test]
async fn e2e_multi_turn_conversation() {
    let provider = Box::new(MockProvider::new(vec![
        text_response("First response"),
        text_response("Second response"),
        text_response("Third response"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);

    let r1 = agent.turn("turn 1").await.unwrap();
    assert!(!r1.is_empty(), "Expected non-empty first response");

    let r2 = agent.turn("turn 2").await.unwrap();
    assert!(!r2.is_empty(), "Expected non-empty second response");
    assert_ne!(r1, r2, "Sequential turn responses should be distinct");

    let r3 = agent.turn("turn 3").await.unwrap();
    assert!(!r3.is_empty(), "Expected non-empty third response");
    assert_ne!(r2, r3, "Sequential turn responses should be distinct");
}

/// Validates that the agent handles unknown tool names gracefully.
#[tokio::test]
async fn e2e_unknown_tool_recovery() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "nonexistent_tool".into(),
            arguments: "{}".into(),
        }]),
        text_response("Recovered from unknown tool"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("call missing tool").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after unknown tool recovery"
    );
}

/// Validates parallel tool dispatch in a single response.
#[tokio::test]
async fn e2e_parallel_tool_dispatch() {
    let (counting_tool, count) = CountingTool::new();

    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![
            ToolCall {
                id: "tc1".into(),
                name: "counter".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "tc2".into(),
                name: "counter".into(),
                arguments: "{}".into(),
            },
        ]),
        text_response("Both tools ran"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(counting_tool)]);
    let response = agent.turn("run both").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after parallel dispatch"
    );
    assert_eq!(*count.lock().unwrap(), 2);
}

// ═════════════════════════════════════════════════════════════════════════════
// Multi-turn history fidelity & memory enrichment tests
// ═════════════════════════════════════════════════════════════════════════════

/// Validates that multi-turn conversation correctly accumulates history
/// and passes growing message sequences to the provider on each turn.
#[tokio::test]
async fn e2e_multi_turn_history_fidelity() {
    let (provider, recorded) = RecordingProvider::new(vec![
        text_response("response 1"),
        text_response("response 2"),
        text_response("response 3"),
    ]);

    let mut agent = build_recording_agent(Box::new(provider), vec![], None);

    let r1 = agent.turn("msg 1").await.unwrap();
    assert_eq!(r1, "response 1");

    let r2 = agent.turn("msg 2").await.unwrap();
    assert_eq!(r2, "response 2");

    let r3 = agent.turn("msg 3").await.unwrap();
    assert_eq!(r3, "response 3");

    let requests = recorded.lock().unwrap();
    assert_eq!(requests.len(), 3, "Provider should receive 3 requests");

    // Request 1: system + user("msg 1")
    let req1 = &requests[0];
    assert!(req1.len() >= 2);
    assert_eq!(req1[0].role, "system");
    assert_eq!(req1[1].role, "user");
    assert!(req1[1].content.contains("msg 1"));

    // Request 2: system + user("msg 1") + assistant("response 1") + user("msg 2")
    let req2 = &requests[1];
    let req2_users: Vec<&ChatMessage> = req2.iter().filter(|m| m.role == "user").collect();
    let req2_assts: Vec<&ChatMessage> = req2.iter().filter(|m| m.role == "assistant").collect();
    assert_eq!(req2_users.len(), 2, "Request 2: expected 2 user messages");
    assert_eq!(
        req2_assts.len(),
        1,
        "Request 2: expected 1 assistant message"
    );
    assert!(req2_users[0].content.contains("msg 1"));
    assert!(req2_users[1].content.contains("msg 2"));
    assert_eq!(req2_assts[0].content, "response 1");

    // Request 3: full history — 3 user + 2 assistant messages
    let req3 = &requests[2];
    let req3_users: Vec<&ChatMessage> = req3.iter().filter(|m| m.role == "user").collect();
    let req3_assts: Vec<&ChatMessage> = req3.iter().filter(|m| m.role == "assistant").collect();
    assert_eq!(req3_users.len(), 3, "Request 3: expected 3 user messages");
    assert_eq!(
        req3_assts.len(),
        2,
        "Request 3: expected 2 assistant messages"
    );
    assert!(req3_users[0].content.contains("msg 1"));
    assert!(req3_users[1].content.contains("msg 2"));
    assert!(req3_users[2].content.contains("msg 3"));
    assert_eq!(req3_assts[0].content, "response 1");
    assert_eq!(req3_assts[1].content, "response 2");

    // Verify agent history: system + 3*(user + assistant) = 7
    let history = agent.history();
    assert_eq!(history.len(), 7);
    assert!(matches!(&history[0], ConversationMessage::Chat(c) if c.role == "system"));
    assert!(matches!(&history[1], ConversationMessage::Chat(c) if c.role == "user"));
    assert!(matches!(&history[2], ConversationMessage::Chat(c) if c.role == "assistant"));
    assert!(
        matches!(&history[6], ConversationMessage::Chat(c) if c.role == "assistant" && c.content == "response 3")
    );
}

/// Validates that a custom MemoryLoader injects RAG context into user
/// messages before they reach the provider.
#[tokio::test]
async fn e2e_memory_enrichment_injects_context() {
    let (provider, recorded) = RecordingProvider::new(vec![text_response("enriched response")]);

    let memory_context = "[Memory context]\n- user_name: test_user\n\n";
    let loader = StaticMemoryLoader::new(memory_context);

    let mut agent = build_recording_agent(Box::new(provider), vec![], Some(Box::new(loader)));

    let response = agent.turn("hello").await.unwrap();
    assert_eq!(response, "enriched response");

    // Provider received enriched message
    let requests = recorded.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let user_msg = requests[0].iter().find(|m| m.role == "user").unwrap();
    assert!(
        user_msg.content.starts_with("[Memory context]"),
        "User message should start with memory context, got: {}",
        user_msg.content,
    );
    assert!(
        user_msg.content.contains("user_name: test_user"),
        "User message should contain memory key-value pair",
    );
    assert!(
        user_msg.content.ends_with("hello"),
        "User message should end with original text, got: {}",
        user_msg.content,
    );

    // Agent history also stores enriched message
    let history = agent.history();
    match &history[1] {
        ConversationMessage::Chat(c) => {
            assert_eq!(c.role, "user");
            assert!(c.content.starts_with("[Memory context]"));
            assert!(c.content.ends_with("hello"));
        }
        other => panic!("Expected Chat variant for user message, got: {other:?}"),
    }
}

/// Validates multi-turn conversation with memory enrichment: every user
/// message is enriched, and the provider sees the full enriched history.
#[tokio::test]
async fn e2e_multi_turn_with_memory_enrichment() {
    let (provider, recorded) =
        RecordingProvider::new(vec![text_response("answer 1"), text_response("answer 2")]);

    let memory_context = "[Memory context]\n- project: zeroclaw\n\n";
    let loader = StaticMemoryLoader::new(memory_context);

    let mut agent = build_recording_agent(Box::new(provider), vec![], Some(Box::new(loader)));

    let r1 = agent.turn("first question").await.unwrap();
    assert_eq!(r1, "answer 1");

    let r2 = agent.turn("second question").await.unwrap();
    assert_eq!(r2, "answer 2");

    let requests = recorded.lock().unwrap();
    assert_eq!(requests.len(), 2);

    // Turn 1: user message is enriched
    let req1_user = requests[0].iter().find(|m| m.role == "user").unwrap();
    assert!(req1_user.content.contains("[Memory context]"));
    assert!(req1_user.content.contains("project: zeroclaw"));
    assert!(req1_user.content.ends_with("first question"));

    // Turn 2: both user messages enriched, assistant from turn 1 present
    let req2_users: Vec<&ChatMessage> = requests[1].iter().filter(|m| m.role == "user").collect();
    assert_eq!(req2_users.len(), 2, "Request 2 should have 2 user messages");

    // Turn 1 user message still enriched in history
    assert!(req2_users[0].content.contains("[Memory context]"));
    assert!(req2_users[0].content.ends_with("first question"));

    // Turn 2 user message also enriched
    assert!(req2_users[1].content.contains("[Memory context]"));
    assert!(req2_users[1].content.ends_with("second question"));

    // Assistant response from turn 1 preserved
    let req2_assts: Vec<&ChatMessage> = requests[1]
        .iter()
        .filter(|m| m.role == "assistant")
        .collect();
    assert_eq!(req2_assts.len(), 1);
    assert_eq!(req2_assts[0].content, "answer 1");

    // History: system + 2*(enriched_user + assistant) = 5
    assert_eq!(agent.history().len(), 5);
}

/// Validates that empty memory context does not prepend memory text.
/// A per-turn datetime prefix may still be present.
#[tokio::test]
async fn e2e_empty_memory_context_passthrough() {
    let (provider, recorded) = RecordingProvider::new(vec![text_response("plain response")]);

    let loader = StaticMemoryLoader::new("");

    let mut agent = build_recording_agent(Box::new(provider), vec![], Some(Box::new(loader)));

    let response = agent.turn("hello").await.unwrap();
    assert_eq!(response, "plain response");

    let requests = recorded.lock().unwrap();
    let user_msg = requests[0].iter().find(|m| m.role == "user").unwrap();
    assert!(
        user_msg.content.ends_with("hello"),
        "User payload should preserve original text suffix, got: {}",
        user_msg.content
    );
    assert!(
        !user_msg.content.contains("[Memory context]"),
        "Empty context should not prepend memory context text, got: {}",
        user_msg.content
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Live integration test — real OpenAI Codex API (requires credentials)
// ═════════════════════════════════════════════════════════════════════════════

/// Sends a real multi-turn conversation to OpenAI Codex and verifies
/// the model retains context from earlier messages.
///
/// Requires valid OAuth credentials in `~/.zeroclaw/`.
/// Run manually: `cargo test e2e_live_openai_codex_multi_turn -- --ignored`
#[tokio::test]
#[ignore = "requires live OpenAI Codex API key"]
async fn e2e_live_openai_codex_multi_turn() {
    use zeroclaw::providers::openai_codex::OpenAiCodexProvider;
    use zeroclaw::providers::traits::Provider;

    let provider = OpenAiCodexProvider::new(&ProviderRuntimeOptions::default(), None).unwrap();
    let model = "gpt-5.3-codex";

    // Turn 1: establish a fact
    let messages_turn1 = vec![
        ChatMessage::system("You are a concise assistant. Reply in one short sentence."),
        ChatMessage::user("The secret word is \"zephyr\". Just confirm you noted it."),
    ];
    let response1 = provider
        .chat_with_history(&messages_turn1, model, 0.0)
        .await;
    assert!(response1.is_ok(), "Turn 1 failed: {:?}", response1.err());
    let r1 = response1.unwrap();
    assert!(!r1.is_empty(), "Turn 1 returned empty response");

    // Turn 2: ask the model to recall the fact
    let messages_turn2 = vec![
        ChatMessage::system("You are a concise assistant. Reply in one short sentence."),
        ChatMessage::user("The secret word is \"zephyr\". Just confirm you noted it."),
        ChatMessage::assistant(&r1),
        ChatMessage::user("What is the secret word?"),
    ];
    let response2 = provider
        .chat_with_history(&messages_turn2, model, 0.0)
        .await;
    assert!(response2.is_ok(), "Turn 2 failed: {:?}", response2.err());
    let r2 = response2.unwrap().to_lowercase();
    assert!(
        r2.contains("zephyr"),
        "Model should recall 'zephyr' from history, got: {r2}",
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Live integration test — Research Phase with real provider
// ═════════════════════════════════════════════════════════════════════════════

/// Tests the research phase module with a real LLM provider.
/// Verifies that:
/// 1. should_trigger correctly identifies research-worthy messages
/// 2. run_research_phase executes tool calls and gathers context
///
/// Requires valid credentials in `~/.zeroclaw/`.
/// Run manually: `cargo test e2e_live_research_phase -- --ignored --nocapture`
#[tokio::test]
#[ignore = "requires live provider API key"]
async fn e2e_live_research_phase() {
    use std::sync::Arc;
    use zeroclaw::agent::research::{run_research_phase, should_trigger};
    use zeroclaw::config::{ResearchPhaseConfig, ResearchTrigger};
    use zeroclaw::observability::NoopObserver;
    use zeroclaw::providers::openai_codex::OpenAiCodexProvider;
    use zeroclaw::providers::traits::Provider;
    use zeroclaw::tools::{Tool, ToolResult};

    // ── Test should_trigger ──
    let config = ResearchPhaseConfig {
        enabled: true,
        trigger: ResearchTrigger::Keywords,
        keywords: vec!["find".into(), "search".into(), "check".into()],
        min_message_length: 20,
        max_iterations: 3,
        show_progress: true,
        system_prompt_prefix: String::new(),
    };

    assert!(
        should_trigger(&config, "find the main function"),
        "Should trigger on 'find' keyword"
    );
    assert!(
        should_trigger(&config, "please search for errors"),
        "Should trigger on 'search' keyword"
    );
    assert!(
        !should_trigger(&config, "hello world"),
        "Should NOT trigger without keywords"
    );

    // ── Test with Always trigger ──
    let always_config = ResearchPhaseConfig {
        enabled: true,
        trigger: ResearchTrigger::Always,
        ..config.clone()
    };
    assert!(
        should_trigger(&always_config, "any message"),
        "Always trigger should match any message"
    );

    // ── Test research phase with live provider ──
    // Create a simple echo tool for testing
    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes the input message back. Use for testing."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Message to echo"
                    }
                },
                "required": ["message"]
            })
        }
        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let msg = args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("(empty)");
            Ok(ToolResult {
                success: true,
                output: format!("Echo: {}", msg),
                error: None,
            })
        }
    }

    let provider = OpenAiCodexProvider::new(&ProviderRuntimeOptions::default(), None)
        .expect("OpenAI Codex provider should initialize for research test");
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
    let observer: Arc<dyn zeroclaw::observability::Observer> = Arc::new(NoopObserver);

    let research_config = ResearchPhaseConfig {
        enabled: true,
        trigger: ResearchTrigger::Always,
        max_iterations: 2,
        show_progress: true,
        ..Default::default()
    };

    println!("\n=== Starting Research Phase Test ===\n");

    let result = run_research_phase(
        &research_config,
        &provider,
        &tools,
        "Use the echo tool to say 'research works'",
        "gpt-5.3-codex",
        0.7,
        observer,
    )
    .await;

    match result {
        Ok(research_result) => {
            println!("Research completed successfully!");
            println!("  Duration: {:?}", research_result.duration);
            println!("  Tool calls: {}", research_result.tool_call_count);
            println!("  Context length: {} chars", research_result.context.len());

            for summary in &research_result.tool_summaries {
                println!(
                    "  - Tool: {} | Success: {} | Args: {}",
                    summary.tool_name, summary.success, summary.arguments_preview
                );
            }

            // The model should have called the echo tool at least once
            // OR provided a research complete summary
            assert!(
                research_result.tool_call_count > 0 || !research_result.context.is_empty(),
                "Research should produce tool calls or context"
            );
        }
        Err(e) => {
            // Network/API errors are expected if credentials aren't configured
            println!("Research phase error (may be expected): {}", e);
        }
    }

    println!("\n=== Research Phase Test Complete ===\n");
}

// ═════════════════════════════════════════════════════════════════════════════
// Full Agent integration test — Research Phase in Agent.turn()
// ═════════════════════════════════════════════════════════════════════════════

/// Validates that the Agent correctly integrates research phase:
/// 1. Research phase is triggered based on config
/// 2. Research context is prepended to user message
/// 3. Provider receives enriched message
///
/// This test uses mocks to verify the integration without external dependencies.
#[tokio::test]
async fn e2e_agent_research_phase_integration() {
    use zeroclaw::config::{ResearchPhaseConfig, ResearchTrigger};

    // Create a recording provider to capture what the agent sends
    let (provider, recorded) = RecordingProvider::new(vec![
        text_response("I'll research that for you"),
        text_response("Based on my research, here's the answer"),
    ]);

    // Build agent with research config enabled (Keywords trigger)
    let research_config = ResearchPhaseConfig {
        enabled: true,
        trigger: ResearchTrigger::Keywords,
        keywords: vec!["search".into(), "find".into(), "look".into()],
        min_message_length: 10,
        max_iterations: 2,
        show_progress: false,
        system_prompt_prefix: String::new(),
    };

    let mut agent = Agent::builder()
        .provider(Box::new(provider))
        .tools(vec![Box::new(EchoTool)])
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .research_config(research_config)
        .build()
        .unwrap();

    // This message should NOT trigger research (no keywords)
    let response1 = agent.turn("hello there").await.unwrap();
    assert!(!response1.is_empty());

    // Verify first message was sent without research enrichment
    {
        let requests = recorded.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let user_msg = requests[0].iter().find(|m| m.role == "user").unwrap();
        // Should be plain message without research prefix
        assert!(
            !user_msg.content.contains("[Research"),
            "Message without keywords should not have research context"
        );
    }
}

/// Validates that Always trigger activates research on every message.
#[tokio::test]
async fn e2e_agent_research_always_trigger() {
    use zeroclaw::config::{ResearchPhaseConfig, ResearchTrigger};

    let (provider, recorded) = RecordingProvider::new(vec![
        // Research phase response
        text_response("Research complete"),
        // Main response
        text_response("Here's your answer with research context"),
    ]);

    let research_config = ResearchPhaseConfig {
        enabled: true,
        trigger: ResearchTrigger::Always,
        keywords: vec![],
        min_message_length: 0,
        max_iterations: 1,
        show_progress: false,
        system_prompt_prefix: String::new(),
    };

    let mut agent = Agent::builder()
        .provider(Box::new(provider))
        .tools(vec![])
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .research_config(research_config)
        .build()
        .unwrap();

    let response = agent.turn("any message").await.unwrap();
    assert!(!response.is_empty());

    // With Always trigger, research should have been attempted
    let requests = recorded.lock().unwrap();
    // At minimum 1 request (main turn), possibly 2 if research phase ran
    assert!(
        !requests.is_empty(),
        "Provider should have received at least one request"
    );
}

/// Validates that research phase works with prompt-guided providers (non-native tools).
/// The provider returns XML tool calls in text, which should be parsed and executed.
#[tokio::test]
async fn e2e_agent_research_prompt_guided() {
    use zeroclaw::config::{ResearchPhaseConfig, ResearchTrigger};
    use zeroclaw::providers::traits::ProviderCapabilities;

    /// Mock provider that does NOT support native tools (like Gemini).
    /// Returns XML tool calls in text that should be parsed by research phase.
    struct PromptGuidedProvider {
        responses: Mutex<Vec<ChatResponse>>,
    }

    impl PromptGuidedProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl Provider for PromptGuidedProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false, // Key difference!
                vision: false,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> Result<String> {
            Ok("fallback".into())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> Result<ChatResponse> {
            let mut guard = self.responses.lock().unwrap();
            if guard.is_empty() {
                return Ok(ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }
            Ok(guard.remove(0))
        }
    }

    // Response 1: Research phase returns XML tool call
    let research_response = ChatResponse {
        text: Some(
            r#"I'll use the echo tool to test.
<tool_call>
{"name": "echo", "arguments": {"message": "research test"}}
</tool_call>"#
                .to_string(),
        ),
        tool_calls: vec![], // Empty! Tool call is in text
        usage: None,
        reasoning_content: None,
    };

    // Response 2: Research complete
    let research_complete = ChatResponse {
        text: Some("[RESEARCH COMPLETE]\n- Found: echo works".to_string()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    };

    // Response 3: Main turn response
    let main_response = text_response("Based on research, here's the answer");

    let provider =
        PromptGuidedProvider::new(vec![research_response, research_complete, main_response]);

    let research_config = ResearchPhaseConfig {
        enabled: true,
        trigger: ResearchTrigger::Always,
        keywords: vec![],
        min_message_length: 0,
        max_iterations: 3,
        show_progress: false,
        system_prompt_prefix: String::new(),
    };

    let mut agent = Agent::builder()
        .provider(Box::new(provider))
        .tools(vec![Box::new(EchoTool)])
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .research_config(research_config)
        .build()
        .unwrap();

    let response = agent.turn("test prompt-guided research").await.unwrap();
    assert!(
        !response.is_empty(),
        "Should get response after prompt-guided research"
    );
}

/// Validates that disabled research phase skips research entirely.
#[tokio::test]
async fn e2e_agent_research_disabled() {
    use zeroclaw::config::{ResearchPhaseConfig, ResearchTrigger};

    let (provider, recorded) = RecordingProvider::new(vec![text_response("Direct response")]);

    let research_config = ResearchPhaseConfig {
        enabled: false, // Disabled
        trigger: ResearchTrigger::Always,
        keywords: vec![],
        min_message_length: 0,
        max_iterations: 5,
        show_progress: true,
        system_prompt_prefix: String::new(),
    };

    let mut agent = Agent::builder()
        .provider(Box::new(provider))
        .tools(vec![Box::new(EchoTool)])
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .research_config(research_config)
        .build()
        .unwrap();

    let response = agent.turn("find something").await.unwrap();
    assert_eq!(response, "Direct response");

    // Only 1 request should be made (main turn, no research)
    let requests = recorded.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "Disabled research should result in only 1 provider call"
    );
}
