//! System-level tests — full agent orchestration with real components.
//!
//! These tests wire ALL internal components together:
//! MockModelProvider → Agent → Tools → Memory → Agent response
//!
//! Unlike integration tests, system tests use real memory backends (SQLite)
//! and verify end-to-end data flow across component boundaries.

use crate::support::helpers::{build_agent_with_sqlite_memory, text_response, tool_response};
use crate::support::{CountingTool, EchoTool, MockModelProvider, RecordingTool};
use zeroclaw::providers::ToolCall;

// ═════════════════════════════════════════════════════════════════════════════
// Full-stack system tests
// ═════════════════════════════════════════════════════════════════════════════

/// Simplest system test: inject message → MockModelProvider returns text → verify response.
#[tokio::test]
async fn system_simple_text_response() {
    let model_provider = Box::new(MockModelProvider::new(vec![text_response(
        "System test response",
    )]));

    let temp_dir = tempfile::tempdir().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(model_provider, vec![Box::new(EchoTool)], temp_dir.path());

    let response = agent.turn("hello system").await.unwrap();
    assert_eq!(response, "System test response");
}

/// Full tool execution flow: message → model_provider requests tool → tool executes →
/// result fed back to model_provider → final response.
#[tokio::test]
async fn system_tool_execution_flow() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "system echo test"}"#.into(),
            extra_content: None,
        }]),
        text_response("Echo returned: system echo test"),
    ]));

    let temp_dir = tempfile::tempdir().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(model_provider, vec![Box::new(EchoTool)], temp_dir.path());

    let response = agent.turn("run echo").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected response after tool execution flow"
    );
}

/// Multi-turn conversation with real SQLite memory — verify history accumulation.
#[tokio::test]
async fn system_multi_turn_conversation() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        text_response("First system response"),
        text_response("Second system response"),
        text_response("Third system response"),
    ]));

    let temp_dir = tempfile::tempdir().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(model_provider, vec![Box::new(EchoTool)], temp_dir.path());

    let r1 = agent.turn("turn 1").await.unwrap();
    assert_eq!(r1, "First system response");

    let r2 = agent.turn("turn 2").await.unwrap();
    assert_eq!(r2, "Second system response");

    let r3 = agent.turn("turn 3").await.unwrap();
    assert_eq!(r3, "Third system response");

    // Verify history accumulated across turns
    let history = agent.history();
    // system + 3*(user + assistant) = 7
    assert_eq!(history.len(), 7, "History should contain 7 messages");
}

/// Tool execution is recorded and arguments are passed correctly.
#[tokio::test]
async fn system_tool_arguments_passed_correctly() {
    let (recording_tool, calls) = RecordingTool::new("recorder");

    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "recorder".into(),
            arguments: r#"{"input": "test_value_42"}"#.into(),
            extra_content: None,
        }]),
        text_response("Tool recorded the input"),
    ]));

    let temp_dir = tempfile::tempdir().unwrap();
    let mut agent = build_agent_with_sqlite_memory(
        model_provider,
        vec![Box::new(recording_tool)],
        temp_dir.path(),
    );

    let response = agent.turn("record something").await.unwrap();
    assert!(!response.is_empty());

    let recorded_calls = calls.lock().unwrap();
    assert_eq!(
        recorded_calls.len(),
        1,
        "Tool should be called exactly once"
    );
    assert_eq!(
        recorded_calls[0]["input"].as_str().unwrap(),
        "test_value_42",
        "Tool should receive correct arguments"
    );
}

/// Multiple tools in a single response — both execute and results feed back.
#[tokio::test]
async fn system_parallel_tool_execution() {
    let (counting_tool, count) = CountingTool::new();

    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![
            ToolCall {
                id: "tc1".into(),
                name: "echo".into(),
                arguments: r#"{"message": "first"}"#.into(),
                extra_content: None,
            },
            ToolCall {
                id: "tc2".into(),
                name: "counter".into(),
                arguments: "{}".into(),
                extra_content: None,
            },
        ]),
        text_response("Both tools completed"),
    ]));

    let temp_dir = tempfile::tempdir().unwrap();
    let mut agent = build_agent_with_sqlite_memory(
        model_provider,
        vec![Box::new(EchoTool), Box::new(counting_tool)],
        temp_dir.path(),
    );

    let response = agent.turn("run both tools").await.unwrap();
    assert_eq!(response, "Both tools completed");
    assert_eq!(*count.lock().unwrap(), 1, "Counter should be invoked once");
}
