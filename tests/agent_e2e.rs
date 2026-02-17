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
use zeroclaw::config::MemoryConfig;
use zeroclaw::memory;
use zeroclaw::memory::Memory;
use zeroclaw::observability::{NoopObserver, Observer};
use zeroclaw::providers::{ChatRequest, ChatResponse, Provider, ToolCall};
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
        (Self { count: count.clone() }, count)
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
    Arc::from(memory::create_memory(&cfg, std::path::Path::new("/tmp"), None).unwrap())
}

fn make_observer() -> Arc<dyn Observer> {
    Arc::from(NoopObserver {})
}

fn text_response(text: &str) -> ChatResponse {
    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![],
    }
}

fn tool_response(calls: Vec<ToolCall>) -> ChatResponse {
    ChatResponse {
        text: Some(String::new()),
        tool_calls: calls,
    }
}

fn build_agent(
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
) -> Agent {
    Agent::builder()
        .provider(provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .unwrap()
}

fn build_agent_xml(
    provider: Box<dyn Provider>,
    tools: Vec<Box<dyn Tool>>,
) -> Agent {
    Agent::builder()
        .provider(provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(XmlToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .unwrap()
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
    assert_eq!(response, "Hello from mock provider");
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
    assert_eq!(response, "Tool executed successfully");
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
    assert_eq!(response, "Done after 2 tool calls");
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
        },
        text_response("XML tool executed"),
    ]));

    let mut agent = build_agent_xml(provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("test xml dispatch").await.unwrap();
    assert_eq!(response, "XML tool executed");
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
    assert_eq!(r1, "First response");

    let r2 = agent.turn("turn 2").await.unwrap();
    assert_eq!(r2, "Second response");

    let r3 = agent.turn("turn 3").await.unwrap();
    assert_eq!(r3, "Third response");
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
    assert_eq!(response, "Recovered from unknown tool");
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
    assert_eq!(response, "Both tools ran");
    assert_eq!(*count.lock().unwrap(), 2);
}
