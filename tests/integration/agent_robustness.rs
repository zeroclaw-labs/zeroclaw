//! TG4: Agent Loop Robustness Tests

use crate::support::helpers::{build_agent, text_response, tool_response};
use crate::support::{CountingTool, EchoTool, FailingTool, MockModelProvider};
use zeroclaw::providers::{ChatResponse, ToolCall};

// ═════════════════════════════════════════════════════════════════════════════
// TG4.1: Malformed tool call recovery
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_recovers_from_text_with_xml_residue() {
    let model_provider = Box::new(MockModelProvider::new(vec![text_response(
        "Here is the result. Some leftover </tool_call> text after.",
    )]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("test").await.unwrap();
    assert!(
        !response.is_empty(),
        "agent should produce non-empty response despite XML residue"
    );
}

#[tokio::test]
async fn agent_handles_tool_call_with_empty_arguments() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("Tool with empty args executed"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("call with empty args").await.unwrap();
    assert!(!response.is_empty());
}

#[tokio::test]
async fn agent_handles_nonexistent_tool_gracefully() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "absolutely_nonexistent_tool".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("Recovered from unknown tool"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("call missing tool").await.unwrap();
    assert!(
        !response.is_empty(),
        "agent should recover from unknown tool"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// TG4.2: Tool failure cascade handling
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_handles_failing_tool() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "failing_tool".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("Tool failed but I recovered"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(FailingTool)]);
    let response = agent.turn("use failing tool").await.unwrap();
    assert!(
        !response.is_empty(),
        "agent should produce response even after tool failure"
    );
}

#[tokio::test]
async fn agent_handles_mixed_tool_success_and_failure() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![
            ToolCall {
                id: "tc1".into(),
                name: "echo".into(),
                arguments: r#"{"message": "success"}"#.into(),
                extra_content: None,
            },
            ToolCall {
                id: "tc2".into(),
                name: "failing_tool".into(),
                arguments: "{}".into(),
                extra_content: None,
            },
        ]),
        text_response("Mixed results processed"),
    ]));

    let mut agent = build_agent(
        model_provider,
        vec![Box::new(EchoTool), Box::new(FailingTool)],
    );
    let response = agent.turn("mixed tools").await.unwrap();
    assert!(!response.is_empty());
}

// ═════════════════════════════════════════════════════════════════════════════
// TG4.3: Iteration limit enforcement
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_respects_max_tool_iterations() {
    let (counting_tool, count) = CountingTool::new();

    // Create 20 tool call responses - more than the default limit of 10
    let mut responses: Vec<ChatResponse> = (0..20)
        .map(|i| {
            tool_response(vec![ToolCall {
                id: format!("tc_{i}"),
                name: "counter".into(),
                arguments: "{}".into(),
                extra_content: None,
            }])
        })
        .collect();
    // Add a final text response that would be used if limit is reached
    responses.push(text_response("Final response after iterations"));

    let model_provider = Box::new(MockModelProvider::new(responses));
    let mut agent = build_agent(model_provider, vec![Box::new(counting_tool)]);

    // Agent should complete (either by hitting iteration limit or running out of responses)
    let result = agent.turn("keep calling tools").await;
    // The agent should complete without hanging
    assert!(result.is_ok() || result.is_err());

    let invocations = *count.lock().unwrap();
    assert!(
        invocations <= 10,
        "tool invocations ({invocations}) should not exceed default max_tool_iterations (10)"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// TG4.4: Empty and whitespace responses
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_handles_empty_provider_response() {
    let model_provider = Box::new(MockModelProvider::new(vec![ChatResponse {
        text: Some(String::new()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    // Should not panic
    let _result = agent.turn("test").await;
}

#[tokio::test]
async fn agent_handles_none_text_response() {
    let model_provider = Box::new(MockModelProvider::new(vec![ChatResponse {
        text: None,
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    let _result = agent.turn("test").await;
}

#[tokio::test]
async fn agent_handles_whitespace_only_response() {
    let model_provider = Box::new(MockModelProvider::new(vec![text_response("   \n\t  ")]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    let _result = agent.turn("test").await;
}

// ═════════════════════════════════════════════════════════════════════════════
// TG4.5: Tool call with special content
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn agent_handles_unicode_tool_arguments() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "こんにちは世界 🌍"}"#.into(),
            extra_content: None,
        }]),
        text_response("Unicode tool executed"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("unicode test").await.unwrap();
    assert!(!response.is_empty());
}

#[tokio::test]
async fn agent_handles_nested_json_tool_arguments() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "{\"nested\": true, \"deep\": {\"level\": 3}}"}"#.into(),
            extra_content: None,
        }]),
        text_response("Nested JSON tool executed"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("nested json test").await.unwrap();
    assert!(!response.is_empty());
}

#[tokio::test]
async fn agent_handles_sequential_tool_then_text() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "step 1"}"#.into(),
            extra_content: None,
        }]),
        text_response("Final answer after tool"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("two step").await.unwrap();
    assert!(
        !response.is_empty(),
        "should produce final text after tool execution"
    );
}
