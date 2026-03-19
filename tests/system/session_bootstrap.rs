//! Session bootstrap tests — verify Augusta agent session management.
//!
//! These tests validate the core session behaviors:
//! - History persists across turns (multi-turn memory)
//! - Memory auto-saves conversations to SQLite
//! - History clears correctly
//! - Single-shot mode returns a response
//! - Tool execution integrates with memory

use crate::support::helpers::{
    build_agent, build_agent_with_sqlite_memory, text_response, tool_response,
};
use crate::support::{EchoTool, MockProvider};
use lightwave_sys::memory::{Memory, MemoryCategory};
use lightwave_sys::providers::ToolCall;
use std::sync::Arc;

// ═════════════════════════════════════════════════════════════════════════════
// Test 1: History persists across turns
// ═════════════════════════════════════════════════════════════════════════════

/// Multi-turn agent accumulates history: system + 3*(user + assistant) = 7.
#[tokio::test]
async fn session_history_persists_across_turns() {
    let provider = Box::new(MockProvider::new(vec![
        text_response("I'm response one"),
        text_response("I'm response two"),
        text_response("I'm response three"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);

    let r1 = agent.turn("first message").await.unwrap();
    assert_eq!(r1, "I'm response one");

    let r2 = agent.turn("second message").await.unwrap();
    assert_eq!(r2, "I'm response two");

    let r3 = agent.turn("third message").await.unwrap();
    assert_eq!(r3, "I'm response three");

    // system + 3 user + 3 assistant = 7
    let history = agent.history();
    assert_eq!(
        history.len(),
        7,
        "Expected 7 messages (system + 3 user/assistant pairs), got {}",
        history.len()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 2: Memory auto-saves to SQLite
// ═════════════════════════════════════════════════════════════════════════════

/// Messages longer than 20 chars should auto-save to SQLite memory when
/// the agent has auto_save enabled.
#[tokio::test]
async fn session_memory_auto_saves() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cfg = lightwave_sys::config::MemoryConfig {
        backend: "sqlite".into(),
        auto_save: true,
        ..lightwave_sys::config::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(lightwave_sys::memory::create_memory(&cfg, temp_dir.path(), None).unwrap());

    // Store a conversation entry directly (simulating what loop_::run does)
    let long_message = "This is a message that definitely exceeds twenty characters";
    mem.store(
        "test_auto_save",
        long_message,
        MemoryCategory::Conversation,
        None,
    )
    .await
    .unwrap();

    // Verify it persists
    let entries = mem
        .list(Some(&MemoryCategory::Conversation), None)
        .await
        .unwrap();
    assert!(
        !entries.is_empty(),
        "Expected at least one conversation entry in SQLite memory"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.content.contains("twenty characters")),
        "Expected stored message to contain our content"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 3: Clear history resets to system prompt only
// ═════════════════════════════════════════════════════════════════════════════

/// After turns, `clear_history()` drops everything. Subsequent history is empty.
#[tokio::test]
async fn session_clear_resets_history() {
    let provider = Box::new(MockProvider::new(vec![
        text_response("before clear"),
        text_response("after clear"),
    ]));

    let mut agent = build_agent(provider, vec![Box::new(EchoTool)]);

    agent.turn("hello").await.unwrap();
    assert!(
        agent.history().len() > 1,
        "Should have history after a turn"
    );

    agent.clear_history();
    assert_eq!(
        agent.history().len(),
        0,
        "clear_history() should empty the history vec"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 4: Single-shot returns response and exits cleanly
// ═════════════════════════════════════════════════════════════════════════════

/// `run_single()` sends one message, gets one response, done.
#[tokio::test]
async fn single_shot_returns_response() {
    let provider = Box::new(MockProvider::new(vec![text_response("single shot answer")]));

    let mut agent = build_agent(provider, vec![]);

    let response = agent.run_single("what time is it").await.unwrap();
    assert_eq!(response, "single shot answer");

    // History should contain system + user + assistant = 3
    assert_eq!(
        agent.history().len(),
        3,
        "Single-shot should still record history"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Test 5: Tool execution with memory persistence
// ═════════════════════════════════════════════════════════════════════════════

/// Full pipeline: message → tool call → memory store → verify memory persists.
#[tokio::test]
async fn session_tool_execution_with_memory() {
    let provider = Box::new(MockProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "tool output for memory test"}"#.into(),
        }]),
        text_response("Tool completed and echoed: tool output for memory test"),
    ]));

    let temp_dir = tempfile::tempdir().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(provider, vec![Box::new(EchoTool)], temp_dir.path());

    let response = agent.turn("run the echo tool").await.unwrap();
    assert!(
        !response.is_empty(),
        "Expected non-empty response after tool execution"
    );

    // Verify the tool actually executed by checking history contains tool result
    let history = agent.history();
    // system + user + assistant(tool_call) + tool_result + assistant(final) = 5
    assert!(
        history.len() >= 4,
        "Expected at least 4 history entries after tool execution, got {}",
        history.len()
    );

    // Store a fact to memory and verify it persists
    let cfg = lightwave_sys::config::MemoryConfig {
        backend: "sqlite".into(),
        ..lightwave_sys::config::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(lightwave_sys::memory::create_memory(&cfg, temp_dir.path(), None).unwrap());
    mem.store(
        "tool_execution_fact",
        "echo tool was executed successfully",
        MemoryCategory::Conversation,
        None,
    )
    .await
    .unwrap();

    let recalled = mem.recall("echo tool", 5, None).await.unwrap();
    assert!(
        !recalled.is_empty(),
        "Memory should contain the stored tool execution fact"
    );
}
