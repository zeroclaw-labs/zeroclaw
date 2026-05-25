//! End-to-end integration tests for agent orchestration.
//!
//! These tests exercise the full agent turn cycle through the public API,
//! using mock model_providers and tools to validate orchestration behavior without
//! external service dependencies. They complement the unit tests in
//! `src/agent/tests.rs` by running at the integration test boundary.
//!
//! Ref: <https://github.com/zeroclaw-labs/zeroclaw/issues/618> (item 6)

use crate::support::helpers::{
    StaticMemoryLoader, build_agent, build_agent_xml, build_recording_agent, text_response,
    tool_response,
};
use crate::support::{CountingTool, EchoTool, MockModelProvider, RecordingModelProvider};
use zeroclaw::providers::traits::ChatMessage;
use zeroclaw::providers::{ChatResponse, ConversationMessage, ToolCall};

// ═════════════════════════════════════════════════════════════════════════════
// E2E smoke tests — full agent turn cycle
// ═════════════════════════════════════════════════════════════════════════════

/// Validates the simplest happy path: user message → LLM text response.
#[tokio::test]
async fn e2e_simple_text_response() {
    let model_provider = Box::new(MockModelProvider::new(vec![text_response(
        "Hello from mock model_provider",
    )]));
    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);

    let response = agent.turn("hi").await.unwrap();
    assert!(!response.is_empty(), "Expected non-empty text response");
}

/// Validates single tool call → tool execution → final LLM response.
#[tokio::test]
async fn e2e_single_tool_call_cycle() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "hello from tool"}"#.into(),
            extra_content: None,
        }]),
        text_response("Tool executed successfully"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
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

    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "counter".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        tool_response(vec![ToolCall {
            id: "tc2".into(),
            name: "counter".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("Done after 2 tool calls"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(counting_tool)]);
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
    let model_provider = Box::new(MockModelProvider::new(vec![
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

    let mut agent = build_agent_xml(model_provider, vec![Box::new(EchoTool)]);
    let response = agent.turn("test xml dispatch").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response from XML dispatcher"
    );
}

/// Validates that multiple sequential turns maintain conversation coherence.
#[tokio::test]
async fn e2e_multi_turn_conversation() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        text_response("First response"),
        text_response("Second response"),
        text_response("Third response"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);

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
    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "nonexistent_tool".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("Recovered from unknown tool"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(EchoTool)]);
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

    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![
            ToolCall {
                id: "tc1".into(),
                name: "counter".into(),
                arguments: "{}".into(),
                extra_content: None,
            },
            ToolCall {
                id: "tc2".into(),
                name: "counter".into(),
                arguments: "{}".into(),
                extra_content: None,
            },
        ]),
        text_response("Both tools ran"),
    ]));

    let mut agent = build_agent(model_provider, vec![Box::new(counting_tool)]);
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
/// and passes growing message sequences to the model_provider on each turn.
#[tokio::test]
async fn e2e_multi_turn_history_fidelity() {
    let (model_provider, recorded) = RecordingModelProvider::new(vec![
        text_response("response 1"),
        text_response("response 2"),
        text_response("response 3"),
    ]);

    let mut agent = build_recording_agent(Box::new(model_provider), vec![], None);

    let r1 = agent.turn("msg 1").await.unwrap();
    assert_eq!(r1, "response 1");

    let r2 = agent.turn("msg 2").await.unwrap();
    assert_eq!(r2, "response 2");

    let r3 = agent.turn("msg 3").await.unwrap();
    assert_eq!(r3, "response 3");

    let requests = recorded.lock().unwrap();
    assert_eq!(requests.len(), 3, "ModelProvider should receive 3 requests");

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
/// messages before they reach the model_provider.
#[tokio::test]
async fn e2e_memory_enrichment_injects_context() {
    let (model_provider, recorded) =
        RecordingModelProvider::new(vec![text_response("enriched response")]);

    let memory_context = "[Memory context]\n- user_name: test_user\n[/Memory context]\n\n";
    let loader = StaticMemoryLoader::new(memory_context);

    let mut agent = build_recording_agent(Box::new(model_provider), vec![], Some(Box::new(loader)));

    let response = agent.turn("hello").await.unwrap();
    assert_eq!(response, "enriched response");

    // ModelProvider received enriched message
    let requests = recorded.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let user_msg = requests[0].iter().find(|m| m.role == "user").unwrap();
    assert!(
        user_msg.content.contains("[Memory context]"),
        "User message should contain memory context, got: {}",
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
            assert!(c.content.contains("[Memory context]"));
            assert!(c.content.ends_with("hello"));
        }
        other => panic!("Expected Chat variant for user message, got: {other:?}"),
    }
}

/// Validates multi-turn conversation with memory enrichment: every user
/// message is enriched, and the model_provider sees the full enriched history.
#[tokio::test]
async fn e2e_multi_turn_with_memory_enrichment() {
    let (model_provider, recorded) =
        RecordingModelProvider::new(vec![text_response("answer 1"), text_response("answer 2")]);

    let memory_context = "[Memory context]\n- project: zeroclaw\n[/Memory context]\n\n";
    let loader = StaticMemoryLoader::new(memory_context);

    let mut agent = build_recording_agent(Box::new(model_provider), vec![], Some(Box::new(loader)));

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
    let (model_provider, recorded) =
        RecordingModelProvider::new(vec![text_response("plain response")]);

    let loader = StaticMemoryLoader::new("");

    let mut agent = build_recording_agent(Box::new(model_provider), vec![], Some(Box::new(loader)));

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
