//! Comprehensive agent-loop test suite.
//!
//! Tests exercise the full `Agent.turn()` cycle with mock providers and tools,
//! covering every edge case an agentic tool loop must handle:
//!
//!   1. Simple text response (no tools)
//!   2. Single tool call → final response
//!   3. Multi-step tool chain (tool A → tool B → response)
//!   4. Max-iteration bailout
//!   5. Unknown tool name recovery
//!   6. Tool execution failure recovery
//!   7. Parallel tool dispatch
//!   8. History trimming during long conversations
//!   9. Memory auto-save round-trip
//!  10. Native vs XML dispatcher integration
//!  11. Empty / whitespace-only LLM responses
//!  12. Mixed text + tool call responses
//!  13. Multi-tool batch in a single response
//!  14. System prompt generation & tool instructions
//!  15. Context enrichment from memory loader
//!  16. ConversationMessage serialization round-trip
//!  17. Tool call with stringified JSON arguments
//!  18. Conversation history fidelity (tool call → tool result → assistant)
//!  19. Builder validation (missing required fields)
//!  20. Idempotent system prompt insertion

use crate::agent::agent::Agent;
use crate::agent::dispatcher::{
    NativeToolDispatcher, ToolDispatcher, ToolExecutionResult, XmlToolDispatcher,
};
use crate::observability::{NoopObserver, Observer};
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use daemonclaw_config::schema::{AgentConfig, MemoryConfig};
use daemonclaw_memory::{self, Memory};
use daemonclaw_providers::{
    ChatMessage, ChatRequest, ChatResponse, ConversationMessage, Provider, ToolCall,
    ToolResultMessage,
};

// ═══════════════════════════════════════════════════════════════════════════
// Test Helpers — Mock Provider, Mock Tool, Mock Memory
// ═══════════════════════════════════════════════════════════════════════════

/// A mock LLM provider that returns pre-scripted responses in order.
/// When the queue is exhausted it returns a simple "done" text response.
struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
    /// Records every request for assertion.
    requests: Mutex<Vec<Vec<ChatMessage>>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        }
    }

    #[allow(dead_code)]
    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
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
        self.requests
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

/// A mock provider that always returns an error.
struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        anyhow::bail!("provider error")
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        anyhow::bail!("provider error")
    }
}

/// A simple echo tool that returns its arguments as output.
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes the input"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
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

/// A tool that always fails execution.
struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "fail"
    }

    fn description(&self) -> &str {
        "Always fails"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("intentional failure".into()),
        })
    }
}

/// A tool that panics (tests error propagation).
struct PanickingTool;

#[async_trait]
impl Tool for PanickingTool {
    fn name(&self) -> &str {
        "panicker"
    }

    fn description(&self) -> &str {
        "Panics on execution"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        anyhow::bail!("catastrophic tool failure")
    }
}

/// A tool that tracks how many times it was called.
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
        "Counts calls"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
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

fn make_memory() -> Arc<dyn Memory> {
    let cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    Arc::from(daemonclaw_memory::create_memory(&cfg, &std::env::temp_dir(), None).unwrap())
}

fn make_sqlite_memory() -> (Arc<dyn Memory>, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = MemoryConfig {
        backend: "sqlite".into(),
        ..MemoryConfig::default()
    };
    let mem = Arc::from(daemonclaw_memory::create_memory(&cfg, tmp.path(), None).unwrap());
    (mem, tmp)
}

fn make_observer() -> Arc<dyn Observer> {
    Arc::from(NoopObserver {})
}

fn build_agent_with(
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    dispatcher: Box<dyn ToolDispatcher>,
) -> Agent {
    Agent::builder()
        .provider(provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(dispatcher)
        .workspace_dir(std::env::temp_dir())
        .build()
        .unwrap()
}

fn build_agent_with_memory(
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    mem: Arc<dyn Memory>,
    auto_save: bool,
) -> Agent {
    Agent::builder()
        .provider(provider)
        .tools(tools)
        .memory(mem)
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .auto_save(auto_save)
        .build()
        .unwrap()
}

fn build_agent_with_config(
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
    config: AgentConfig,
) -> Agent {
    Agent::builder()
        .provider(provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .config(config)
        .build()
        .unwrap()
}

/// Helper: create a ChatResponse with tool calls (native format).
fn tool_response(calls: Vec<ToolCall>) -> ChatResponse {
    ChatResponse {
        text: Some(String::new()),
        tool_calls: calls,
        usage: None,
        reasoning_content: None,
    }
}

/// Helper: create a plain text ChatResponse.
fn text_response(text: &str) -> ChatResponse {
    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }
}

/// Helper: create an XML-style tool call response.
fn xml_tool_response(name: &str, args: &str) -> ChatResponse {
    ChatResponse {
        text: Some(format!(
            "<tool_call>\n{{\"name\": \"{name}\", \"arguments\": {args}}}\n</tool_call>"
        )),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. Simple text response (no tools)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_returns_text_when_no_tools_called() {
    let provider = Box::new(ScriptedProvider::new(vec![text_response("Hello world")]));
    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(NativeToolDispatcher),
    );

    let response = agent.turn("hi").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty text response from provider"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Single tool call → final response
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_executes_single_tool_then_returns() {
    let provider = Box::new(ScriptedProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "hello from tool"}"#.into(),
        }]),
        text_response("I ran the tool"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(NativeToolDispatcher),
    );

    let response = agent.turn("run echo").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after tool execution"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Multi-step tool chain (tool A → tool B → response)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_handles_multi_step_tool_chain() {
    let (counting_tool, count) = CountingTool::new();

    let provider = Box::new(ScriptedProvider::new(vec![
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
        tool_response(vec![ToolCall {
            id: "tc3".into(),
            name: "counter".into(),
            arguments: "{}".into(),
        }]),
        text_response("Done after 3 calls"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(counting_tool)],
        Box::new(NativeToolDispatcher),
    );

    let response = agent.turn("count 3 times").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after multi-step chain"
    );
    assert_eq!(*count.lock().unwrap(), 3);
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. (Removed: max-iteration bailout — iteration limit replaced by token budget)
// ═══════════════════════════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════════════════════════
// 5. Unknown tool name recovery
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_handles_unknown_tool_gracefully() {
    let provider = Box::new(ScriptedProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "nonexistent_tool".into(),
            arguments: "{}".into(),
        }]),
        text_response("I couldn't find that tool"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(NativeToolDispatcher),
    );

    let response = agent.turn("use nonexistent").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after unknown tool recovery"
    );

    // Verify the tool result mentioned "Unknown tool"
    let has_tool_result = agent.history().iter().any(|msg| match msg {
        ConversationMessage::ToolResults(results) => {
            results.iter().any(|r| r.content.contains("Unknown tool"))
        }
        _ => false,
    });
    assert!(
        has_tool_result,
        "Expected tool result with 'Unknown tool' message"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. Tool execution failure recovery
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_recovers_from_tool_failure() {
    let provider = Box::new(ScriptedProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "fail".into(),
            arguments: "{}".into(),
        }]),
        text_response("Tool failed but I recovered"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(FailingTool)],
        Box::new(NativeToolDispatcher),
    );

    let response = agent.turn("try failing tool").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after tool failure recovery"
    );
}

#[tokio::test]
async fn turn_recovers_from_tool_error() {
    let provider = Box::new(ScriptedProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "panicker".into(),
            arguments: "{}".into(),
        }]),
        text_response("I recovered from the error"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(PanickingTool)],
        Box::new(NativeToolDispatcher),
    );

    let response = agent.turn("try panicking").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after tool error recovery"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. Provider error propagation
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_propagates_provider_error() {
    let mut agent = build_agent_with(
        Box::new(FailingProvider),
        vec![],
        Box::new(NativeToolDispatcher),
    );

    let result = agent.turn("hello").await;
    assert!(result.is_err(), "Expected provider error to propagate");
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. History trimming during long conversations
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn history_trims_after_max_messages() {
    let max_history = 6;
    let mut responses = vec![];
    for _ in 0..max_history + 5 {
        responses.push(text_response("ok"));
    }

    let provider = Box::new(ScriptedProvider::new(responses));
    let config = AgentConfig {
        max_history_messages: max_history,
        ..AgentConfig::default()
    };

    let mut agent = build_agent_with_config(provider, vec![], config);

    for i in 0..max_history + 5 {
        let _ = agent.turn(&format!("msg {i}")).await.unwrap();
    }

    // System prompt (1) + trimmed messages
    // Should not exceed max_history + 1 (system prompt)
    assert!(
        agent.history().len() <= max_history + 1,
        "History length {} exceeds max {} + 1 (system)",
        agent.history().len(),
        max_history,
    );

    // System prompt should always be preserved
    let first = &agent.history()[0];
    assert!(matches!(first, ConversationMessage::Chat(c) if c.role == "system"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. Memory auto-save round-trip
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn auto_save_stores_only_user_messages_in_memory() {
    let (mem, _tmp) = make_sqlite_memory();
    let provider = Box::new(ScriptedProvider::new(vec![text_response(
        "I remember everything",
    )]));

    let mut agent = build_agent_with_memory(
        provider,
        vec![],
        mem.clone(),
        true, // auto_save enabled
    );

    let _ = agent.turn("Remember this fact").await.unwrap();

    // Auto-save only persists user-stated input, never assistant-generated summaries.
    let count = mem.count().await.unwrap();
    assert_eq!(
        count, 1,
        "Expected exactly 1 user memory entry, got {count}"
    );

    let stored = mem.get("user_msg").await.unwrap();
    assert!(stored.is_some(), "Expected user_msg key to be present");
    assert_eq!(
        stored.unwrap().content,
        "Remember this fact",
        "Stored memory should match the original user message"
    );

    let assistant = mem.get("assistant_resp").await.unwrap();
    assert!(
        assistant.is_none(),
        "assistant_resp should not be auto-saved anymore"
    );
}

#[tokio::test]
async fn auto_save_disabled_does_not_store() {
    let (mem, _tmp) = make_sqlite_memory();
    let provider = Box::new(ScriptedProvider::new(vec![text_response("hello")]));

    let mut agent = build_agent_with_memory(
        provider,
        vec![],
        mem.clone(),
        false, // auto_save disabled
    );

    let _ = agent.turn("test message").await.unwrap();

    let count = mem.count().await.unwrap();
    assert_eq!(count, 0, "Expected 0 memory entries with auto_save off");
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. Native vs XML dispatcher integration
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn xml_dispatcher_parses_and_loops() {
    let provider = Box::new(ScriptedProvider::new(vec![
        xml_tool_response("echo", r#"{"message": "xml-test"}"#),
        text_response("XML tool completed"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(XmlToolDispatcher),
    );

    let response = agent.turn("test xml").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response from XML dispatcher"
    );
}

#[tokio::test]
async fn native_dispatcher_sends_tool_specs() {
    let provider = Box::new(ScriptedProvider::new(vec![text_response("ok")]));
    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(NativeToolDispatcher),
    );

    let _ = agent.turn("hi").await.unwrap();

    // NativeToolDispatcher.should_send_tool_specs() returns true
    let dispatcher = NativeToolDispatcher;
    assert!(dispatcher.should_send_tool_specs());
}

#[tokio::test]
async fn xml_dispatcher_does_not_send_tool_specs() {
    let dispatcher = XmlToolDispatcher;
    assert!(!dispatcher.should_send_tool_specs());
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. Empty / whitespace-only LLM responses
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_handles_empty_text_response() {
    let provider = Box::new(ScriptedProvider::new(vec![ChatResponse {
        text: Some(String::new()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }]));

    let mut agent = build_agent_with(provider, vec![], Box::new(NativeToolDispatcher));

    let response = agent.turn("hi").await.unwrap();
    assert!(response.is_empty());
}

#[tokio::test]
async fn turn_handles_none_text_response() {
    let provider = Box::new(ScriptedProvider::new(vec![ChatResponse {
        text: None,
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }]));

    let mut agent = build_agent_with(provider, vec![], Box::new(NativeToolDispatcher));

    // Should not panic — falls back to empty string
    let response = agent.turn("hi").await.unwrap();
    assert!(response.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. Mixed text + tool call responses
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_preserves_text_alongside_tool_calls() {
    let provider = Box::new(ScriptedProvider::new(vec![
        ChatResponse {
            text: Some("Let me check...".into()),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "echo".into(),
                arguments: r#"{"message": "hi"}"#.into(),
            }],
            usage: None,
            reasoning_content: None,
        },
        text_response("Here are the results"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(NativeToolDispatcher),
    );

    let response = agent.turn("check something").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty final response after mixed text+tool"
    );

    // The intermediate text should be in history
    let has_intermediate = agent.history().iter().any(|msg| match msg {
        ConversationMessage::Chat(c) => c.role == "assistant" && c.content.contains("Let me check"),
        _ => false,
    });
    assert!(has_intermediate, "Intermediate text should be in history");
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. Multi-tool batch in a single response
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn turn_handles_multiple_tools_in_one_response() {
    let (counting_tool, count) = CountingTool::new();

    let provider = Box::new(ScriptedProvider::new(vec![
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
            ToolCall {
                id: "tc3".into(),
                name: "counter".into(),
                arguments: "{}".into(),
            },
        ]),
        text_response("All 3 done"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(counting_tool)],
        Box::new(NativeToolDispatcher),
    );

    let response = agent.turn("batch").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after multi-tool batch"
    );
    assert_eq!(
        *count.lock().unwrap(),
        3,
        "All 3 tools should have been called"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. System prompt generation & tool instructions
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn system_prompt_injected_on_first_turn() {
    let provider = Box::new(ScriptedProvider::new(vec![text_response("ok")]));
    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(NativeToolDispatcher),
    );

    assert!(agent.history().is_empty(), "History should start empty");

    let _ = agent.turn("hi").await.unwrap();

    // First message should be the system prompt
    let first = &agent.history()[0];
    assert!(
        matches!(first, ConversationMessage::Chat(c) if c.role == "system"),
        "First history entry should be system prompt"
    );
}

#[tokio::test]
async fn system_prompt_not_duplicated_on_second_turn() {
    let provider = Box::new(ScriptedProvider::new(vec![
        text_response("first"),
        text_response("second"),
    ]));
    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(NativeToolDispatcher),
    );

    let _ = agent.turn("hi").await.unwrap();
    let _ = agent.turn("hello again").await.unwrap();

    let system_count = agent
        .history()
        .iter()
        .filter(|msg| matches!(msg, ConversationMessage::Chat(c) if c.role == "system"))
        .count();
    assert_eq!(system_count, 1, "System prompt should appear exactly once");
}

// ═══════════════════════════════════════════════════════════════════════════
// 15. Conversation history fidelity
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn history_contains_all_expected_entries_after_tool_loop() {
    let provider = Box::new(ScriptedProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "tool-out"}"#.into(),
        }]),
        text_response("final answer"),
    ]));

    let mut agent = build_agent_with(
        provider,
        vec![Box::new(EchoTool)],
        Box::new(NativeToolDispatcher),
    );

    let _ = agent.turn("test").await.unwrap();

    // Expected history entries:
    //   0: system prompt
    //   1: user message "test"
    //   2: AssistantToolCalls
    //   3: ToolResults
    //   4: assistant "final answer"
    let history = agent.history();
    assert!(
        history.len() >= 5,
        "Expected at least 5 history entries, got {}",
        history.len()
    );

    assert!(matches!(&history[0], ConversationMessage::Chat(c) if c.role == "system"));
    assert!(matches!(&history[1], ConversationMessage::Chat(c) if c.role == "user"));
    assert!(matches!(
        &history[2],
        ConversationMessage::AssistantToolCalls { .. }
    ));
    assert!(matches!(&history[3], ConversationMessage::ToolResults(_)));
    assert!(
        matches!(&history[4], ConversationMessage::Chat(c) if c.role == "assistant" && c.content == "final answer")
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 16. Builder validation
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn builder_fails_without_provider() {
    let result = Agent::builder()
        .tools(vec![])
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build();

    assert!(result.is_err(), "Building without provider should fail");
}

// ═══════════════════════════════════════════════════════════════════════════
// 17. Multi-turn conversation maintains context
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn multi_turn_maintains_growing_history() {
    let provider = Box::new(ScriptedProvider::new(vec![
        text_response("response 1"),
        text_response("response 2"),
        text_response("response 3"),
    ]));

    let mut agent = build_agent_with(provider, vec![], Box::new(NativeToolDispatcher));

    let r1 = agent.turn("msg 1").await.unwrap();
    let len_after_1 = agent.history().len();

    let r2 = agent.turn("msg 2").await.unwrap();
    let len_after_2 = agent.history().len();

    let r3 = agent.turn("msg 3").await.unwrap();
    let len_after_3 = agent.history().len();

    assert_eq!(r1, "response 1");
    assert_eq!(r2, "response 2");
    assert_eq!(r3, "response 3");

    // History should grow with each turn (user + assistant per turn)
    assert!(
        len_after_2 > len_after_1,
        "History should grow after turn 2"
    );
    assert!(
        len_after_3 > len_after_2,
        "History should grow after turn 3"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 18. Tool call with stringified JSON arguments (common LLM pattern)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn native_dispatcher_handles_stringified_arguments() {
    let dispatcher = NativeToolDispatcher;
    let response = ChatResponse {
        text: Some(String::new()),
        tool_calls: vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "hello"}"#.into(),
        }],
        usage: None,
        reasoning_content: None,
    };

    let (_, calls) = dispatcher.parse_response(&response);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "echo");
    assert_eq!(
        calls[0].arguments.get("message").unwrap().as_str().unwrap(),
        "hello"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 19. XML dispatcher edge cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn xml_dispatcher_handles_nested_json() {
    let response = ChatResponse {
        text: Some(
            r#"<tool_call>
{"name": "file_write", "arguments": {"path": "test.json", "content": "{\"key\": \"value\"}"}}
</tool_call>"#
                .into(),
        ),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    };

    let dispatcher = XmlToolDispatcher;
    let (_, calls) = dispatcher.parse_response(&response);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "file_write");
    assert_eq!(
        calls[0].arguments.get("path").unwrap().as_str().unwrap(),
        "test.json"
    );
}

#[test]
fn xml_dispatcher_handles_empty_tool_call_tag() {
    let response = ChatResponse {
        text: Some("<tool_call>\n</tool_call>\nSome text".into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    };

    let dispatcher = XmlToolDispatcher;
    let (text, calls) = dispatcher.parse_response(&response);
    assert!(calls.is_empty());
    assert!(text.contains("Some text"));
}

#[test]
fn xml_dispatcher_handles_unclosed_tool_call() {
    let response = ChatResponse {
        text: Some("Before\n<tool_call>\n{\"name\": \"shell\"}".into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    };

    let dispatcher = XmlToolDispatcher;
    let (text, calls) = dispatcher.parse_response(&response);
    // Should not panic — just treat as text
    assert!(calls.is_empty());
    assert!(text.contains("Before"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 20. ConversationMessage serialization round-trip
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn conversation_message_serialization_roundtrip() {
    let messages = vec![
        ConversationMessage::Chat(ChatMessage::system("system")),
        ConversationMessage::Chat(ChatMessage::user("hello")),
        ConversationMessage::AssistantToolCalls {
            text: Some("checking".into()),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
            }],
            reasoning_content: None,
        },
        ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "tc1".into(),
            content: "ok".into(),
        }]),
        ConversationMessage::Chat(ChatMessage::assistant("done")),
    ];

    for msg in &messages {
        let json = serde_json::to_string(msg).unwrap();
        let parsed: ConversationMessage = serde_json::from_str(&json).unwrap();

        // Verify the variant type matches
        match (msg, &parsed) {
            (ConversationMessage::Chat(a), ConversationMessage::Chat(b)) => {
                assert_eq!(a.role, b.role);
                assert_eq!(a.content, b.content);
            }
            (
                ConversationMessage::AssistantToolCalls {
                    text: a_text,
                    tool_calls: a_calls,
                    ..
                },
                ConversationMessage::AssistantToolCalls {
                    text: b_text,
                    tool_calls: b_calls,
                    ..
                },
            ) => {
                assert_eq!(a_text, b_text);
                assert_eq!(a_calls.len(), b_calls.len());
            }
            (ConversationMessage::ToolResults(a), ConversationMessage::ToolResults(b)) => {
                assert_eq!(a.len(), b.len());
            }
            _ => panic!("Variant mismatch after serialization"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 21. Tool dispatcher format_results
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn xml_format_results_includes_status_and_output() {
    let dispatcher = XmlToolDispatcher;
    let results = vec![
        ToolExecutionResult {
            name: "shell".into(),
            output: "file1.txt\nfile2.txt".into(),
            success: true,
            tool_call_id: None,
        },
        ToolExecutionResult {
            name: "file_read".into(),
            output: "Error: file not found".into(),
            success: false,
            tool_call_id: None,
        },
    ];

    let msg = dispatcher.format_results(&results);
    let content = match msg {
        ConversationMessage::Chat(c) => c.content,
        _ => panic!("Expected Chat variant"),
    };

    assert!(content.contains("shell"));
    assert!(content.contains("file1.txt"));
    assert!(content.contains("ok"));
    assert!(content.contains("file_read"));
    assert!(content.contains("error"));
}

#[test]
fn native_format_results_maps_tool_call_ids() {
    let dispatcher = NativeToolDispatcher;
    let results = vec![
        ToolExecutionResult {
            name: "a".into(),
            output: "out1".into(),
            success: true,
            tool_call_id: Some("tc-001".into()),
        },
        ToolExecutionResult {
            name: "b".into(),
            output: "out2".into(),
            success: true,
            tool_call_id: Some("tc-002".into()),
        },
    ];

    let msg = dispatcher.format_results(&results);
    match msg {
        ConversationMessage::ToolResults(r) => {
            assert_eq!(r.len(), 2);
            assert_eq!(r[0].tool_call_id, "tc-001");
            assert_eq!(r[0].content, "out1");
            assert_eq!(r[1].tool_call_id, "tc-002");
            assert_eq!(r[1].content, "out2");
        }
        _ => panic!("Expected ToolResults"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 22. to_provider_messages conversion
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn xml_dispatcher_converts_history_to_provider_messages() {
    let dispatcher = XmlToolDispatcher;
    let history = vec![
        ConversationMessage::Chat(ChatMessage::system("sys")),
        ConversationMessage::Chat(ChatMessage::user("hi")),
        ConversationMessage::AssistantToolCalls {
            text: Some("checking".into()),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
            }],
            reasoning_content: None,
        },
        ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "tc1".into(),
            content: "ok".into(),
        }]),
        ConversationMessage::Chat(ChatMessage::assistant("done")),
    ];

    let messages = dispatcher.to_provider_messages(&history);

    // Should have: system, user, assistant (from tool calls), user (tool results), assistant
    assert!(messages.len() >= 4);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
}

#[test]
fn native_dispatcher_converts_tool_results_to_tool_messages() {
    let dispatcher = NativeToolDispatcher;
    let history = vec![ConversationMessage::ToolResults(vec![
        ToolResultMessage {
            tool_call_id: "tc1".into(),
            content: "output1".into(),
        },
        ToolResultMessage {
            tool_call_id: "tc2".into(),
            content: "output2".into(),
        },
    ])];

    let messages = dispatcher.to_provider_messages(&history);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "tool");
    assert_eq!(messages[1].role, "tool");
}

// ═══════════════════════════════════════════════════════════════════════════
// 23. XML tool instructions generation
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn xml_dispatcher_generates_tool_instructions() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
    let dispatcher = XmlToolDispatcher;
    let instructions = dispatcher.prompt_instructions(&tools);

    assert!(instructions.contains("## Tool Use Protocol"));
    assert!(instructions.contains("<tool_call>"));
    // Tool listing is handled by ToolsSection in prompt.rs, not by the
    // dispatcher.  prompt_instructions() must only emit the protocol envelope.
    assert!(
        !instructions.contains("echo"),
        "dispatcher should not duplicate tool listing"
    );
}

#[test]
fn native_dispatcher_returns_empty_instructions() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
    let dispatcher = NativeToolDispatcher;
    let instructions = dispatcher.prompt_instructions(&tools);
    assert!(instructions.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 24. Clear history
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn clear_history_resets_conversation() {
    let provider = Box::new(ScriptedProvider::new(vec![
        text_response("first"),
        text_response("second"),
    ]));

    let mut agent = build_agent_with(provider, vec![], Box::new(NativeToolDispatcher));

    let _ = agent.turn("hi").await.unwrap();
    assert!(!agent.history().is_empty());

    agent.clear_history();
    assert!(agent.history().is_empty());

    // Next turn should re-inject system prompt
    let _ = agent.turn("hello again").await.unwrap();
    assert!(matches!(
        &agent.history()[0],
        ConversationMessage::Chat(c) if c.role == "system"
    ));
}

// ═══════════════════════════════════════════════════════════════════════════
// 25. run_single delegates to turn
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn run_single_delegates_to_turn() {
    let provider = Box::new(ScriptedProvider::new(vec![text_response("via run_single")]));
    let mut agent = build_agent_with(provider, vec![], Box::new(NativeToolDispatcher));

    let response = agent.run_single("test").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response from run_single"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Spine-contract tests: [20] TurnCompleteAction, [11] concurrency barriers
// ═══════════════════════════════════════════════════════════════════════════

mod spine_contract {
    use super::*;
    use crate::agent::loop_::{run_tool_call_loop};
    use crate::hooks::{HookHandler, HookRunner, TurnCompleteAction};
    use daemonclaw_api::agent::TurnResult;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // ── Hook that returns a scripted TurnCompleteAction ──────────────

    struct ScriptedTurnHook {
        action: TurnCompleteAction,
    }

    #[async_trait]
    impl HookHandler for ScriptedTurnHook {
        fn name(&self) -> &str {
            "scripted_turn_hook"
        }

        async fn on_turn_complete(&self, _result: &TurnResult) -> TurnCompleteAction {
            self.action.clone()
        }
    }

    fn make_hooks(action: TurnCompleteAction) -> HookRunner {
        let mut runner = HookRunner::new();
        runner.register(Box::new(ScriptedTurnHook { action }));
        runner
    }

    fn default_multimodal() -> daemonclaw_config::schema::MultimodalConfig {
        daemonclaw_config::schema::MultimodalConfig::default()
    }

    // ── [20] Stop halts the loop ────────────────────────────────────
    //
    // Production call site: loop_.rs line 1662-1668
    // The Stop arm returns Ok(...) immediately after fire_turn_complete.
    // Because the hook fires inside `if tool_calls.is_empty()`, which is
    // already the exit path, Stop and Continue both return — the difference
    // is that Stop skips fire_extract_post_turn. We test that the loop
    // exits cleanly with Stop and returns the accumulated text, AND that
    // if we switch the hook to PreventStop, the loop does NOT exit on the
    // first final response (proving the action is actually read).
    #[tokio::test]
    async fn hook_stop_exits_loop_and_prevent_stop_forces_continuation() {
        // Provider returns a final text response (no tool calls).
        let provider = ScriptedProvider::new(vec![
            ChatResponse {
                text: Some("first answer".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
            ChatResponse {
                text: Some("second answer".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]);
        let observer = NoopObserver {};
        let tools_registry: Arc<Vec<Box<dyn Tool>>> = Arc::new(vec![]);
        let multimodal = default_multimodal();

        // With Stop: loop should return after first response.
        let hooks_stop = make_hooks(TurnCompleteAction::Stop);
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("question"),
        ];
        let result = run_tool_call_loop(
            &provider, &mut history, &tools_registry, &observer,
            "mock", "mock-model", 0.0, true, None, "test", None,
            &multimodal, 0, None, None, Some(&hooks_stop),
            &[], &[], None, None,
            &daemonclaw_config::schema::PacingConfig::default(),
            0, 0, None, None, None, None, None,
        ).await.unwrap();

        assert!(
            result.contains("first answer"),
            "Stop should return the first response, got: {result}"
        );
        // Provider should have been called exactly once.
        assert_eq!(provider.request_count(), 1, "Stop should exit after one LLM call");

        // With PreventStop: loop should continue past the first final
        // response and make additional LLM calls (bounded by MAX_HOOK_CONTINUATIONS=5).
        let provider2 = ScriptedProvider::new(vec![
            ChatResponse {
                text: Some("first answer".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
            ChatResponse {
                text: Some("second answer".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]);
        let hooks_prevent = make_hooks(TurnCompleteAction::PreventStop);
        let mut history2 = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("question"),
        ];
        let result2 = run_tool_call_loop(
            &provider2, &mut history2, &tools_registry, &observer,
            "mock", "mock-model", 0.0, true, None, "test", None,
            &multimodal, 0, None, None, Some(&hooks_prevent),
            &[], &[], None, None,
            &daemonclaw_config::schema::PacingConfig::default(),
            0, 0, None, None, None, None, None,
        ).await.unwrap();

        // PreventStop should have forced more than 1 call before the
        // continuation budget (5) is exhausted and the loop exits.
        assert!(
            provider2.request_count() >= 2,
            "PreventStop should force at least 2 LLM calls, got {}",
            provider2.request_count()
        );
    }

    // ── [20] InjectError injects message and continues ──────────────
    //
    // Production call site: loop_.rs line 1675-1678
    // InjectError pushes a user message with "[Hook error] <msg>" and
    // continues the loop. We verify the error appears in history and
    // the loop makes another LLM call rather than exiting.
    #[tokio::test]
    async fn hook_inject_error_lands_in_history_and_loop_continues() {
        let provider = ScriptedProvider::new(vec![
            // First: final response → triggers InjectError hook → loop continues
            ChatResponse {
                text: Some("first answer".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
            // Second: final response after the injected error → loop exits normally
            ChatResponse {
                text: Some("recovered".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]);
        let observer = NoopObserver {};
        let tools_registry: Arc<Vec<Box<dyn Tool>>> = Arc::new(vec![]);
        let multimodal = default_multimodal();

        let hooks = make_hooks(TurnCompleteAction::InjectError(
            "test_sentinel_error_42".into(),
        ));
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("question"),
        ];
        let result = run_tool_call_loop(
            &provider, &mut history, &tools_registry, &observer,
            "mock", "mock-model", 0.0, true, None, "test", None,
            &multimodal, 0, None, None, Some(&hooks),
            &[], &[], None, None,
            &daemonclaw_config::schema::PacingConfig::default(),
            0, 0, None, None, None, None, None,
        ).await.unwrap();

        // The hook fires on every final response and injects an error,
        // causing the loop to continue. The continuation budget (5) eventually
        // exhausts and the loop exits. We assert:
        //
        // 1. The injected error message appeared in history.
        let has_injected = history
            .iter()
            .any(|m| m.role == "user" && m.content.contains("test_sentinel_error_42"));
        assert!(
            has_injected,
            "InjectError should place '[Hook error] test_sentinel_error_42' in history"
        );

        // 2. The provider was called more than once (loop continued, didn't
        //    exit on first final response like Stop would).
        assert!(
            provider.request_count() >= 2,
            "InjectError should cause the loop to continue, got {} calls",
            provider.request_count()
        );

        // 3. Distinguish from Stop: with Stop the provider is called exactly once.
        //    With InjectError it must be called multiple times.
        assert!(
            provider.request_count() > 1,
            "InjectError must differ from Stop (which calls provider exactly once)"
        );
    }

    // ── [11] Exclusive tool forms a barrier ──────────────────────────
    //
    // Production call site: loop_.rs line 1975-1993
    // partition_tool_calls groups consecutive safe tools into parallel
    // batches; non-safe tools form single-item serial barriers.
    // execute_tools_batched processes batches in order.
    //
    // We test by having 3 tools called in one iteration: safe_a, exclusive,
    // safe_b. The exclusive tool should NOT run concurrently with either
    // safe tool. We verify by recording wall-clock overlap.

    struct TimestampTool {
        tool_name: String,
        safe: bool,
        log: Arc<Mutex<Vec<(String, std::time::Instant, std::time::Instant)>>>,
    }

    #[async_trait]
    impl Tool for TimestampTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "Records execution timestamps"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
            self.safe
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
            let start = std::time::Instant::now();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let end = std::time::Instant::now();
            self.log.lock().unwrap().push((
                self.tool_name.clone(),
                start,
                end,
            ));
            Ok(ToolResult {
                success: true,
                output: format!("{} done", self.tool_name),
                error: None,
            })
        }
    }

    // ── [10] End-to-end: stream → executor → mid-stream dispatch ──────
    //
    // Drives a mock stream through consume_provider_streaming_response with
    // Some(executor). The stream emits ToolCall(tool_a), delays 80ms, then
    // emits ToolCall(tool_b). tool_a (50ms execution) must start during the
    // 80ms delay — proving dispatch happens on arrival from the stream, not
    // after the stream completes.
    //
    // Production call site: loop_.rs line 1164-1172, where
    // consume_provider_streaming_response receives Some(&mut streaming_executor).
    // All production callers (run(), orchestrator, delegate) go through
    // run_tool_call_loop which constructs the executor at line 1157-1162 and
    // passes Some at line 1172.
    #[tokio::test]
    async fn streaming_dispatch_starts_tool_before_stream_finishes() {
        use crate::agent::loop_::consume_provider_streaming_response;
        use crate::agent::streaming_executor::StreamingToolExecutor;
        use daemonclaw_providers::traits::{
            ChatRequest, StreamChunk, StreamEvent, StreamOptions,
        };
        use futures_util::stream::StreamExt;

        let log: Arc<Mutex<Vec<(String, std::time::Instant, std::time::Instant)>>> =
            Arc::new(Mutex::new(Vec::new()));

        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(TimestampTool {
                tool_name: "tool_a".into(),
                safe: true,
                log: Arc::clone(&log),
            }),
            Box::new(TimestampTool {
                tool_name: "tool_b".into(),
                safe: true,
                log: Arc::clone(&log),
            }),
        ];
        let tools_arc = Arc::new(tools);

        // Mock provider that streams: ToolCall(tool_a) → 80ms delay → ToolCall(tool_b) → Final
        struct DelayedToolStream;

        #[async_trait]
        impl Provider for DelayedToolStream {
            async fn chat_with_system(
                &self, _: Option<&str>, _: &str, _: &str, _: f64,
            ) -> anyhow::Result<String> {
                unreachable!()
            }
            async fn chat(
                &self, _: ChatRequest<'_>, _: &str, _: f64,
            ) -> anyhow::Result<daemonclaw_providers::ChatResponse> {
                unreachable!()
            }
            fn supports_streaming(&self) -> bool { true }
            fn supports_streaming_tool_events(&self) -> bool { true }
            fn stream_chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: f64,
                _options: StreamOptions,
            ) -> futures_util::stream::BoxStream<
                'static,
                daemonclaw_providers::traits::StreamResult<StreamEvent>,
            > {
                let (tx, rx) = tokio::sync::mpsc::channel(8);
                tokio::spawn(async move {
                    let _ = tx.send(Ok(StreamEvent::ToolCall(daemonclaw_providers::ToolCall {
                        id: "tc1".into(),
                        name: "tool_a".into(),
                        arguments: "{}".into(),
                    }))).await;
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    let _ = tx.send(Ok(StreamEvent::ToolCall(daemonclaw_providers::ToolCall {
                        id: "tc2".into(),
                        name: "tool_b".into(),
                        arguments: "{}".into(),
                    }))).await;
                    let _ = tx.send(Ok(StreamEvent::Final)).await;
                });
                Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
            }
        }

        let provider = DelayedToolStream;
        let messages = vec![ChatMessage::user("run both tools")];
        let observer_arc: Arc<dyn crate::observability::Observer> =
            Arc::new(crate::observability::NoopObserver {});

        let mut executor = StreamingToolExecutor::new(
            Arc::clone(&tools_arc),
            observer_arc,
            None,
            None,
        );

        let before_stream = std::time::Instant::now();

        let outcome = consume_provider_streaming_response(
            &provider,
            &messages,
            None,
            "test-model",
            0.0,
            None,
            None,
            Some(&mut executor),
        )
        .await
        .expect("streaming should succeed");

        // Stream is done. tool_calls should be empty (executor consumed them).
        assert!(
            outcome.tool_calls.is_empty(),
            "executor should have consumed all tool calls from the stream"
        );

        // Collect executor results.
        assert!(executor.has_tools(), "executor should have tools");
        let results = executor.finish().await;
        assert_eq!(results.len(), 2, "both tools should have executed");
        assert!(results.iter().all(|(_, o)| o.success));

        let entries = log.lock().unwrap().clone();
        assert_eq!(entries.len(), 2);

        let tool_a = entries.iter().find(|(n, _, _)| n == "tool_a").unwrap();
        let tool_b = entries.iter().find(|(n, _, _)| n == "tool_b").unwrap();

        // tool_a (50ms execution) must have started during the 80ms delay
        // before tool_b arrived from the stream.
        // tool_b's StreamEvent::ToolCall was emitted at ~before_stream + 80ms.
        let tool_b_stream_arrival = before_stream + std::time::Duration::from_millis(80);

        assert!(
            tool_a.1 < tool_b_stream_arrival,
            "tool_a must start before tool_b arrives from the stream. \
             tool_a started at +{}ms, tool_b arrived at +80ms",
            tool_a.1.duration_since(before_stream).as_millis(),
        );

        // Also confirm tool_a didn't wait for tool_b — its start should be
        // well before the 80ms mark (within the first few ms after stream start).
        assert!(
            tool_a.1.duration_since(before_stream).as_millis() < 30,
            "tool_a should start within 30ms of stream start, got {}ms",
            tool_a.1.duration_since(before_stream).as_millis(),
        );
    }

    // ── [4] Context collapse: reversibility proven ─────────────────
    //
    // Collapse must be reversible: project() produces a summary view,
    // but expand_region() restores the originals on the next project().
    // This distinguishes collapse from lossy compaction.
    #[test]
    fn context_collapse_is_reversible() {
        use crate::agent::context_collapse::ContextCollapser;

        let history = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("question 1"),
            ChatMessage::assistant("answer 1"),
            ChatMessage::user("question 2"),
            ChatMessage::assistant("answer 2"),
            ChatMessage::user("question 3"),
            ChatMessage::assistant("answer 3 — the latest"),
        ];
        let original_len = history.len();

        let mut collapser = ContextCollapser::new(2); // protect last 2

        // Collapse messages 1..5 (the middle conversation, not system or tail)
        collapser.collapse_region(1, 5, "Summary of Q1+A1+Q2+A2".into(), 100);

        // Project: should have system + [COLLAPSED] + question3 + answer3
        let projected = collapser.project(&history);
        assert!(
            projected.len() < original_len,
            "projected should be shorter than original ({} vs {})",
            projected.len(), original_len,
        );
        assert_eq!(projected[0].role, "system");
        assert!(
            projected[1].content.contains("[COLLAPSED"),
            "second message should be collapse summary, got: {}",
            projected[1].content,
        );
        assert!(
            projected[1].content.contains("Summary of Q1+A1+Q2+A2"),
            "collapse summary should contain the provided text",
        );
        // Protected tail preserved
        assert!(projected.last().unwrap().content.contains("answer 3"));

        // Verify original history is UNCHANGED
        assert_eq!(history.len(), original_len);
        assert_eq!(history[1].content, "question 1");
        assert_eq!(history[4].content, "answer 2");

        // Expand: remove the collapse, project again
        collapser.expand_region(1);
        let restored = collapser.project(&history);
        assert_eq!(
            restored.len(), original_len,
            "after expand, projection should equal original length"
        );
        for (i, (orig, rest)) in history.iter().zip(restored.iter()).enumerate() {
            assert_eq!(
                orig.content, rest.content,
                "message {i} should be identical after expand: '{}' vs '{}'",
                orig.content, rest.content,
            );
            assert_eq!(
                orig.role, rest.role,
                "message {i} role should match after expand",
            );
        }
    }

    // ── [4] Context collapse: reversibility under mutation ────────
    //
    // The production path: collapse → loop pushes a new turn to raw history
    // → next iteration computes collapse fresh. This test proves the
    // "computed fresh each iteration" claim holds when raw history is
    // mutated between project() calls.
    #[test]
    fn context_collapse_reversible_across_mutations() {
        use crate::agent::context_collapse::ContextCollapser;

        let mut history = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("question 1"),
            ChatMessage::assistant("answer 1"),
            ChatMessage::user("question 2"),
            ChatMessage::assistant("answer 2"),
        ];

        // Iteration 1: collapse messages 1..3
        let mut collapser1 = ContextCollapser::new(2);
        collapser1.collapse_region(1, 3, "Summary of Q1+A1".into(), 50);
        let projected1 = collapser1.project(&history);
        assert_eq!(projected1.len(), 4); // system + [COLLAPSED] + Q2 + A2
        assert!(projected1[1].content.contains("[COLLAPSED"));

        // Simulate loop mutation: push a new turn (as run_tool_call_loop does)
        history.push(ChatMessage::user("question 3"));
        history.push(ChatMessage::assistant("answer 3"));
        // Raw history now has 7 messages. The old collapser's region (1..3)
        // still refers to valid indices because we only appended.

        // Iteration 2: compute collapse fresh over the mutated history.
        // This is what the production loop does — new collapser each iteration.
        let mut collapser2 = ContextCollapser::new(2);
        // Collapse a larger region now that history grew
        collapser2.collapse_region(1, 5, "Summary of Q1+A1+Q2+A2".into(), 100);
        let projected2 = collapser2.project(&history);
        // system + [COLLAPSED] + Q3 + A3
        assert_eq!(projected2.len(), 4);
        assert!(projected2[1].content.contains("Summary of Q1+A1+Q2+A2"));
        assert_eq!(projected2[2].content, "question 3");
        assert_eq!(projected2[3].content, "answer 3");

        // Expand: originals still intact in raw history
        collapser2.expand_region(1);
        let restored = collapser2.project(&history);
        assert_eq!(restored.len(), history.len());
        for (i, (orig, rest)) in history.iter().zip(restored.iter()).enumerate() {
            assert_eq!(
                orig.content, rest.content,
                "message {i} should be identical after expand across mutations"
            );
        }

        // Also verify the raw history was never modified by any projection
        assert_eq!(history[1].content, "question 1");
        assert_eq!(history[2].content, "answer 1");
        assert_eq!(history[3].content, "question 2");
        assert_eq!(history[4].content, "answer 2");
        assert_eq!(history[5].content, "question 3");
        assert_eq!(history[6].content, "answer 3");
    }

    #[tokio::test]
    async fn exclusive_tool_does_not_overlap_with_safe_tools() {
        use crate::agent::tool_execution::{partition_tool_calls, execute_tools_batched};
        use crate::agent::loop_::ParsedToolCall;

        let log: Arc<Mutex<Vec<(String, std::time::Instant, std::time::Instant)>>> =
            Arc::new(Mutex::new(Vec::new()));

        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(TimestampTool {
                tool_name: "safe_a".into(),
                safe: true,
                log: Arc::clone(&log),
            }),
            Box::new(TimestampTool {
                tool_name: "exclusive".into(),
                safe: false,
                log: Arc::clone(&log),
            }),
            Box::new(TimestampTool {
                tool_name: "safe_b".into(),
                safe: true,
                log: Arc::clone(&log),
            }),
        ];

        let calls = vec![
            ParsedToolCall {
                name: "safe_a".into(),
                arguments: serde_json::json!({}),
                tool_call_id: Some("c1".into()),
            },
            ParsedToolCall {
                name: "exclusive".into(),
                arguments: serde_json::json!({}),
                tool_call_id: Some("c2".into()),
            },
            ParsedToolCall {
                name: "safe_b".into(),
                arguments: serde_json::json!({}),
                tool_call_id: Some("c3".into()),
            },
        ];

        let batches = partition_tool_calls(&calls, &tools, None);

        // Verify partitioning: safe_a alone, exclusive alone, safe_b alone.
        // (safe_a and safe_b are not consecutive — exclusive sits between them)
        assert_eq!(batches.len(), 3, "3 tools with exclusive in the middle should produce 3 batches");
        assert!(batches[0].concurrent, "first batch (safe_a) should be concurrent-capable");
        assert!(!batches[1].concurrent, "second batch (exclusive) should be serial");
        assert!(batches[2].concurrent, "third batch (safe_b) should be concurrent-capable");

        let observer = NoopObserver {};
        let outcomes = execute_tools_batched(
            &calls, &batches, &tools, None, &observer, None, None,
        ).await.unwrap();

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|o| o.success));

        // Verify no temporal overlap between exclusive and either safe tool.
        let entries = log.lock().unwrap().clone();
        assert_eq!(entries.len(), 3, "all 3 tools should have executed");

        let exclusive_entry = entries.iter().find(|(n, _, _)| n == "exclusive").unwrap();
        for (name, start, end) in &entries {
            if name == "exclusive" {
                continue;
            }
            let overlaps = *start < exclusive_entry.2 && *end > exclusive_entry.1;
            assert!(
                !overlaps,
                "Tool '{name}' ({}ms..{}ms) overlapped with exclusive ({}ms..{}ms)",
                start.duration_since(entries[0].1).as_millis(),
                end.duration_since(entries[0].1).as_millis(),
                exclusive_entry.1.duration_since(entries[0].1).as_millis(),
                exclusive_entry.2.duration_since(entries[0].1).as_millis(),
            );
        }
    }

    // ── Model-echo deduplication: response repeated after tool-call iteration ──
    //
    // Reproduces the live v0.7.8 bug: iteration 1 returns text + tool call,
    // iteration 2 echoes the same text (model repetition) with no tool calls.
    // Without the fix, the truncation detector fires (text ends with emoji,
    // not in the punctuation set) and accumulated_display_text doubles.
    // With the fix, the repetition is detected and suppressed.
    #[tokio::test]
    async fn model_echo_after_tool_call_does_not_duplicate_output() {
        // Response must be >500 chars (truncation check floor) and >200 chars
        // (repetition guard floor) to exercise the bug path.
        let response_text = "Here is my full analysis of the upgrade. \
            The history management overhaul is a big deal — that graduated \
            trimming with collapsible summaries fixes the exact brittle behavior \
            we identified in the analysis. No more all-or-nothing context management. \
            The multi-tool pipelining is smart too. The old behavior of waiting for \
            the full model response before executing any tools was dead time. \
            Provider selection works. Loop detection replaces fixed caps. \
            Content tagging for external results is solid defense-in-depth. \
            Ready to test whenever you are \u{1F980}";

        let provider = ScriptedProvider::new(vec![
            // Iteration 1: text + tool call
            ChatResponse {
                text: Some(response_text.into()),
                tool_calls: vec![daemonclaw_providers::ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: r#"{"message": "stored"}"#.into(),
                }],
                usage: None,
                reasoning_content: None,
            },
            // Iteration 2: model echoes the exact same text, no tool calls
            ChatResponse {
                text: Some(response_text.into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]);

        let observer = NoopObserver {};
        let tools_registry: Arc<Vec<Box<dyn Tool>>> = Arc::new(vec![
            Box::new(EchoTool) as Box<dyn Tool>,
        ]);
        let multimodal = default_multimodal();

        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("tell me about the upgrade"),
        ];

        let result = run_tool_call_loop(
            &provider, &mut history, &tools_registry, &observer,
            "mock", "mock-model", 0.0, true, None, "test", None,
            &multimodal, 0, None, None, None,
            &[], &[], None, None,
            &daemonclaw_config::schema::PacingConfig::default(),
            0, 0, None, None, None, None, None,
        ).await.unwrap();

        // The response text should appear exactly once, not doubled.
        let occurrences = result.matches("Provider selection works").count();
        assert_eq!(
            occurrences, 1,
            "Response text should appear exactly once, got {occurrences}. \
             Full result ({} chars): {result}",
            result.len()
        );

        // The text should still be present (not suppressed entirely).
        assert!(
            result.contains("Ready to test"),
            "Response should contain the original text"
        );

        // History surface: the echoed text should NOT appear in history twice.
        // Iteration 1 pushes an assistant message (text + tool call) at line 2402.
        // Iteration 2's echo should be suppressed from history by the repetition guard.
        let assistant_messages: Vec<&ChatMessage> = history
            .iter()
            .filter(|m| m.role == "assistant")
            .collect();
        let history_occurrences = assistant_messages
            .iter()
            .filter(|m| m.content.contains("Provider selection works"))
            .count();
        assert_eq!(
            history_occurrences, 1,
            "Echoed text should appear in history exactly once (from iteration 1), \
             got {history_occurrences}. Assistant messages: {:?}",
            assistant_messages.iter().map(|m| m.content.len()).collect::<Vec<_>>()
        );
    }

    // ── Restate-then-continue: guard must NOT suppress novel content ────
    //
    // Model restates the prior accumulated text as a prefix, then appends
    // substantial new content. The repetition guard must let this through —
    // both the new content and the prefix must survive to delivery and history.
    // Without the novel-tail check, starts_with would flag this as repetition
    // and silently drop the entire response including the new content.
    #[tokio::test]
    async fn restate_then_continue_preserves_novel_tail() {
        let prefix_text = "Here is my full analysis of the upgrade. \
            The history management overhaul is a big deal — that graduated \
            trimming with collapsible summaries fixes the exact brittle behavior \
            we identified in the analysis. No more all-or-nothing context management. \
            The multi-tool pipelining is smart too. The old behavior of waiting for \
            the full model response before executing any tools was dead time. \
            Provider selection works. Loop detection replaces fixed caps. \
            Content tagging for external results is solid defense-in-depth. \
            Ready to test whenever you are \u{1F980}";

        // Iteration 2 restates the prefix, then adds substantial new analysis.
        let novel_content = "\n\nNow that I think about it further, there are \
            three additional considerations worth flagging. First, the provider \
            fallback chain should be tested under real latency conditions — the \
            jittered backoff looks correct on paper but edge cases around \
            simultaneous rate-limit responses from multiple providers could \
            surface ordering issues. Second, the context collapser needs \
            verification that the protect_last_n parameter actually prevents \
            the most recent exchange from being collapsed during aggressive \
            compaction. Third, we should validate that the streaming executor \
            correctly handles a tool that panics mid-execution without poisoning \
            the entire turn. These are all testable in isolation.";

        let full_response = format!("{prefix_text}{novel_content}");

        let provider = ScriptedProvider::new(vec![
            // Iteration 1: prefix text + tool call
            ChatResponse {
                text: Some(prefix_text.into()),
                tool_calls: vec![daemonclaw_providers::ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: r#"{"message": "noted"}"#.into(),
                }],
                usage: None,
                reasoning_content: None,
            },
            // Iteration 2: restates prefix + adds substantial novel content
            ChatResponse {
                text: Some(full_response.clone()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]);

        let observer = NoopObserver {};
        let tools_registry: Arc<Vec<Box<dyn Tool>>> = Arc::new(vec![
            Box::new(EchoTool) as Box<dyn Tool>,
        ]);
        let multimodal = default_multimodal();

        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("analyze the upgrade"),
        ];

        let result = run_tool_call_loop(
            &provider, &mut history, &tools_registry, &observer,
            "mock", "mock-model", 0.0, true, None, "test", None,
            &multimodal, 0, None, None, None,
            &[], &[], None, None,
            &daemonclaw_config::schema::PacingConfig::default(),
            0, 0, None, None, None, None, None,
        ).await.unwrap();

        // The novel content must survive to delivery.
        assert!(
            result.contains("provider fallback chain"),
            "Novel content must survive to delivery. Result ({} chars): {}",
            result.len(), &result[..result.len().min(200)]
        );
        assert!(
            result.contains("protect_last_n parameter"),
            "Novel content (second point) must survive"
        );

        // The novel content must also land in history.
        let assistant_messages: Vec<&ChatMessage> = history
            .iter()
            .filter(|m| m.role == "assistant")
            .collect();
        let has_novel_in_history = assistant_messages
            .iter()
            .any(|m| m.content.contains("provider fallback chain"));
        assert!(
            has_novel_in_history,
            "Novel content must be in history. Assistant message lengths: {:?}",
            assistant_messages.iter().map(|m| m.content.len()).collect::<Vec<_>>()
        );
    }

    /// Regression: the first user message (original ask) must survive all
    /// trimming and pruning stages, even when large tool results push the
    /// conversation past the token/message budget.
    #[test]
    fn first_user_message_survives_aggressive_trimming() {
        use crate::agent::history::{emergency_history_trim, trim_history};
        use crate::agent::history_pruner::{prune_history, HistoryPrunerConfig};
        use daemonclaw_providers::ChatMessage;

        let original_ask = "Research quantum computing applications in drug discovery and \
            report your findings with citations. Focus on molecular simulation, \
            protein folding, and combinatorial optimization for lead compounds.";

        // Simulate a short conversation with large tool results — the exact
        // shape that triggered the live bug (handful of turns, oversized
        // tool results inflating token count).
        let mut history = vec![
            ChatMessage::system("You are a helpful research assistant."),
            ChatMessage::user(original_ask),
        ];

        // Add 20 assistant+tool pairs with large results to blow past budget.
        for i in 0..20 {
            let tool_json = format!(
                r#"{{"content":"searching","tool_calls":[{{"id":"t{i}","name":"web_search","arguments":"{{}}"}}]}}"#
            );
            history.push(ChatMessage {
                role: "assistant".to_string(),
                content: tool_json,
            });
            let result = format!(
                r#"{{"tool_call_id":"t{i}","content":"{}"}}"#,
                "x".repeat(3000)
            );
            history.push(ChatMessage {
                role: "tool".to_string(),
                content: result,
            });
        }
        history.push(ChatMessage::assistant("Here's what I found...".to_string()));

        // 43 messages total: system + user + 20*(assistant+tool) + final assistant.
        assert_eq!(history.len(), 43);

        // Test 1: trim_history with aggressive limit.
        let mut trimmed = history.clone();
        trim_history(&mut trimmed, 10);
        assert!(
            trimmed.iter().any(|m| m.content == original_ask),
            "trim_history must preserve the first user message (original ask)"
        );

        // Test 2: emergency_history_trim (drops 1/3 of messages).
        let mut emergency = history.clone();
        let dropped = emergency_history_trim(&mut emergency, 4);
        assert!(dropped > 0, "emergency trim should have dropped messages");
        assert!(
            emergency.iter().any(|m| m.content == original_ask),
            "emergency_history_trim must preserve the first user message"
        );

        // Test 3: prune_history with very tight token budget.
        let mut pruned = history.clone();
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 200,
            keep_recent: 4,
            collapse_tool_results: true,
        };
        prune_history(&mut pruned, &config);
        assert!(
            pruned.iter().any(|m| m.content == original_ask),
            "prune_history must preserve the first user message even under \
             extreme token pressure. Remaining roles: {:?}",
            pruned.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
        );
    }
}
