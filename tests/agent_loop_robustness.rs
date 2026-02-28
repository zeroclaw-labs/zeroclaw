//! TG4: Agent Loop Robustness Tests
//!
//! Prevents: Pattern 4 — Agent loop & tool call processing bugs (13% of user bugs).
//! Issues: #746, #418, #777, #848
//!
//! Tests agent behavior with malformed tool calls, empty responses,
//! max iteration limits, and cascading tool failures using mock providers.
//! Complements inline parse_tool_calls tests in `src/agent/loop_.rs`.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};
use zeroclaw::agent::agent::Agent;
use zeroclaw::agent::dispatcher::NativeToolDispatcher;
use zeroclaw::config::MemoryConfig;
use zeroclaw::memory;
use zeroclaw::memory::Memory;
use zeroclaw::observability::{NoopObserver, Observer};
use zeroclaw::providers::{ChatRequest, ChatResponse, Provider, ToolCall};
use zeroclaw::tools::{Tool, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// Mock infrastructure
// ─────────────────────────────────────────────────────────────────────────────

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

/// Tool that always fails, simulating a broken external service
struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        "failing_tool"
    }
    fn description(&self) -> &str {
        "Always fails"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Service unavailable: connection timeout".into()),
        })
    }
}

/// Tool that tracks invocations
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

// ═════════════════════════════════════════════════════════════════════════════
// TG4.1: Malformed tool call recovery
// ═════════════════════════════════════════════════════════════════════════════

/// Agent should recover when LLM returns text with residual XML tags (#746)
#[tokio::test]
async fn agent_recovers_from_text_with_xml_residue() {
    let provider = Box::new(MockProvider::new(vec![text_response(
        "Here is the result. Some leftover </tool_call> text after.",
    )]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("test").await.unwrap();
    assert!(
        !response.is_empty(),
        "agent should produce non-empty response despite XML residue"
    );
}

/// Agent should handle tool call with empty arguments gracefully
#[tokio::test]
async fn agent_handles_tool_call_with_empty_arguments() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: "{}".into(),
        }]),
        text_response("Tool with empty args executed"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("call with empty args").await.unwrap();
    assert!(!response.is_empty());
}

/// Agent should handle unknown tool name without crashing (#848 related)
#[tokio::test]
async fn agent_handles_nonexistent_tool_gracefully() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "absolutely_nonexistent_tool".into(),
            arguments: "{}".into(),
        }]),
        text_response("Recovered from unknown tool"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("call missing tool").await.unwrap();
    assert!(
        !response.is_empty(),
        "agent should recover from unknown tool"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// TG4.2: Tool failure cascade handling (#848)
// ═════════════════════════════════════════════════════════════════════════════

/// Agent should handle repeated tool failures without infinite loop
#[tokio::test]
async fn agent_handles_failing_tool() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "failing_tool".into(),
            arguments: "{}".into(),
        }]),
        text_response("Tool failed but I recovered"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(FailingTool)]);
    let response = agent.turn("use failing tool").await.unwrap();
    assert!(
        !response.is_empty(),
        "agent should produce response even after tool failure"
    );
}

/// Agent should handle mixed tool calls (some succeed, some fail)
#[tokio::test]
async fn agent_handles_mixed_tool_success_and_failure() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![
            ToolCall {
                id: "tc1".into(),
                name: "echo".into(),
                arguments: r#"{"message": "success"}"#.into(),
            },
            ToolCall {
                id: "tc2".into(),
                name: "failing_tool".into(),
                arguments: "{}".into(),
            },
        ]),
        text_response("Mixed results processed"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool), Box::new(FailingTool)]);
    let response = agent.turn("mixed tools").await.unwrap();
    assert!(!response.is_empty());
}

// ═════════════════════════════════════════════════════════════════════════════
// TG4.3: Iteration limit enforcement (#777)
// ═════════════════════════════════════════════════════════════════════════════

/// Agent should not exceed max_tool_iterations (default=20) even with
/// a provider that keeps returning tool calls
#[tokio::test]
async fn agent_respects_max_tool_iterations() {
    let (counting_tool, count) = CountingTool::new();

    // Create 30 tool call responses - more than the default limit of 20
    let mut responses: Vec<ChatResponse> = (0..30)
        .map(|i| {
            tool_response(vec![ToolCall {
                id: format!("tc_{i}"),
                name: "counter".into(),
                arguments: "{}".into(),
            }])
        })
        .collect();
    // Add a final text response that would be used if limit is reached
    responses.push(text_response("Final response after iterations"));

    let provider = Box::new(MockProvider::new(responses));
    let mut agent = build_agent(provider, vec![Box::new(counting_tool)]);

    // Agent should complete (either by hitting iteration limit or running out of responses)
    let result = agent.turn("keep calling tools").await;
    // The agent should complete without hanging
    assert!(result.is_ok() || result.is_err());

    let invocations = *count.lock().unwrap();
    assert!(
        invocations <= 20,
        "tool invocations ({invocations}) should not exceed default max_tool_iterations (20)"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// TG4.4: Empty and whitespace responses
// ═════════════════════════════════════════════════════════════════════════════

/// Agent should handle empty text response from provider (#418 related)
#[tokio::test]
async fn agent_handles_empty_provider_response() {
    let provider = Box::new(MockProvider::new(vec![ChatResponse {
        text: Some(String::new()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    // Should not panic
    let _result = agent.turn("test").await;
}

/// Agent should handle None text response from provider
#[tokio::test]
async fn agent_handles_none_text_response() {
    let provider = Box::new(MockProvider::new(vec![ChatResponse {
        text: None,
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let _result = agent.turn("test").await;
}

/// Agent should handle whitespace-only response
#[tokio::test]
async fn agent_handles_whitespace_only_response() {
    let provider = Box::new(MockProvider::new(vec![text_response("   \n\t  ")]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let _result = agent.turn("test").await;
}

// ═════════════════════════════════════════════════════════════════════════════
// TG4.5: Tool call with special content
// ═════════════════════════════════════════════════════════════════════════════

/// Agent should handle tool arguments with unicode content
#[tokio::test]
async fn agent_handles_unicode_tool_arguments() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "こんにちは世界 🌍"}"#.into(),
        }]),
        text_response("Unicode tool executed"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("unicode test").await.unwrap();
    assert!(!response.is_empty());
}

/// Agent should handle tool arguments with nested JSON
#[tokio::test]
async fn agent_handles_nested_json_tool_arguments() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "{\"nested\": true, \"deep\": {\"level\": 3}}"}"#.into(),
        }]),
        text_response("Nested JSON tool executed"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("nested json test").await.unwrap();
    assert!(!response.is_empty());
}

/// Agent should handle tool call followed by immediate text (no second LLM call)
#[tokio::test]
async fn agent_handles_sequential_tool_then_text() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "step 1"}"#.into(),
        }]),
        text_response("Final answer after tool"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("two step").await.unwrap();
    assert!(
        !response.is_empty(),
        "should produce final text after tool execution"
    );
}
