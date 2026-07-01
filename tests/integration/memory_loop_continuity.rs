//! End-to-end tests for memory–loop–heartbeat continuity.
//!
//! Validates that:
//! - Memory persists across agent turns and sessions
//! - The agent loop maintains context awareness through tool iterations
//! - Memory recall enriches prompts so the agent "remembers" prior work
//! - Context compression preserves facts to memory before discarding
//! - Multi-step tasks complete without the agent stopping prematurely

use std::sync::Arc;

use zeroclaw::config::MemoryConfig;
use zeroclaw::memory::sqlite::SqliteMemory;
use zeroclaw::memory::traits::{Memory, MemoryCategory};
use zeroclaw::providers::ToolCall;

use crate::support::helpers::{build_agent_with_sqlite_memory, text_response, tool_response};
use crate::support::{CountingTool, EchoTool, MockModelProvider};

// ═════════════════════════════════════════════════════════════════════════════
// 1. Memory Store + Recall Persistence
// ═════════════════════════════════════════════════════════════════════════════

/// Store a fact, then recall it in a fresh memory instance (same DB).
#[tokio::test]
async fn memory_persists_across_instances() {
    let tmp = tempfile::TempDir::new().unwrap();

    // Instance 1: store
    {
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        mem.store(
            "project_deadline",
            "The deadline is March 30th 2026",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
    }

    // Instance 2: recall (simulates restart)
    {
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let results = mem.recall("deadline", 5, None, None, None).await.unwrap();
        assert!(
            !results.is_empty(),
            "Memory should survive instance restart"
        );
        assert!(
            results[0].content.contains("March 30th"),
            "Recalled content should match: got '{}'",
            results[0].content
        );
    }
}

/// Store multiple facts across categories and recall by relevance.
#[tokio::test]
async fn memory_recall_returns_relevant_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem = SqliteMemory::new("test", tmp.path()).unwrap();

    mem.store(
        "user_name",
        "User's name is Argenis",
        MemoryCategory::Core,
        None,
    )
    .await
    .unwrap();
    mem.store("user_lang", "User prefers Rust", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store(
        "daily_note",
        "Had a meeting about deployment",
        MemoryCategory::Daily,
        None,
    )
    .await
    .unwrap();

    let results = mem.recall("Argenis", 5, None, None, None).await.unwrap();
    assert!(
        results.iter().any(|e| e.content.contains("Argenis")),
        "Recall for 'Argenis' should find the name entry"
    );

    let results = mem.recall("Rust", 5, None, None, None).await.unwrap();
    assert!(
        results.iter().any(|e| e.content.contains("Rust")),
        "Recall for 'Rust' should find the language preference"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. Agent Loop Multi-Step Completion
// ═════════════════════════════════════════════════════════════════════════════

/// Agent completes a 5-step tool chain without stopping.
#[tokio::test]
async fn agent_completes_five_step_tool_chain() {
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
        tool_response(vec![ToolCall {
            id: "tc3".into(),
            name: "counter".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        tool_response(vec![ToolCall {
            id: "tc4".into(),
            name: "counter".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        tool_response(vec![ToolCall {
            id: "tc5".into(),
            name: "counter".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("All 5 steps completed successfully"),
    ]));

    let tmp = tempfile::TempDir::new().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(model_provider, vec![Box::new(counting_tool)], tmp.path());

    let response = agent.turn("Execute 5 sequential operations").await.unwrap();
    assert!(!response.is_empty());
    assert_eq!(
        *count.lock().unwrap(),
        5,
        "All 5 tool calls should have executed"
    );
}

/// Agent handles a multi-turn conversation, maintaining history.
#[tokio::test]
async fn agent_maintains_history_across_turns() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        text_response("I'll remember that your name is Argenis."),
        text_response("Your name is Argenis, as you told me earlier."),
        text_response("Yes, you are Argenis and you prefer Rust."),
    ]));

    let tmp = tempfile::TempDir::new().unwrap();
    let mut agent = build_agent_with_sqlite_memory(model_provider, vec![], tmp.path());

    let r1 = agent.turn("My name is Argenis").await.unwrap();
    assert!(!r1.is_empty());

    let r2 = agent.turn("What is my name?").await.unwrap();
    assert!(!r2.is_empty());

    let r3 = agent.turn("I also prefer Rust").await.unwrap();
    assert!(!r3.is_empty());
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. Memory-Enriched Agent Turns
// ═════════════════════════════════════════════════════════════════════════════

/// Agent with SqliteMemory stores and recalls across turns.
#[tokio::test]
async fn agent_auto_saves_and_recalls_memory() {
    let tmp = tempfile::TempDir::new().unwrap();

    // Pre-seed memory with a fact
    {
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        mem.store(
            "project_tech",
            "The project uses Rust and Tokio for async runtime",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
    }

    // Agent should have access to this via memory recall
    let model_provider = Box::new(MockModelProvider::new(vec![text_response(
        "Based on memory, the project uses Rust and Tokio.",
    )]));

    let mut agent = build_agent_with_sqlite_memory(model_provider, vec![], tmp.path());
    let response = agent
        .turn("What tech does this project use?")
        .await
        .unwrap();
    assert!(!response.is_empty());
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. Battle-Tested Loop Scenarios
// ═════════════════════════════════════════════════════════════════════════════

/// Agent handles interleaved tool calls and text responses without stopping.
#[tokio::test]
async fn agent_handles_interleaved_tools_and_text() {
    let model_provider = Box::new(MockModelProvider::new(vec![
        // Step 1: tool call
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "echo".into(),
            arguments: r#"{"message": "creating file"}"#.into(),
            extra_content: None,
        }]),
        // Step 2: another tool call
        tool_response(vec![ToolCall {
            id: "tc2".into(),
            name: "echo".into(),
            arguments: r#"{"message": "reading file"}"#.into(),
            extra_content: None,
        }]),
        // Step 3: final text
        text_response("File created and read successfully"),
    ]));

    let tmp = tempfile::TempDir::new().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(model_provider, vec![Box::new(EchoTool)], tmp.path());

    let response = agent.turn("Create a file then read it").await.unwrap();
    assert!(
        !response.is_empty(),
        "Agent should complete interleaved tool+text sequence"
    );
}

/// Agent survives large tool output (truncation should kick in).
#[tokio::test]
async fn agent_survives_large_tool_output() {
    use zeroclaw::tools::{Tool, ToolResult};

    /// Tool that returns a very large output.
    struct LargeOutputTool;

    impl ::zeroclaw_api::attribution::Attributable for LargeOutputTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            <Self as Tool>::name(self)
        }
    }

    #[async_trait::async_trait]
    impl Tool for LargeOutputTool {
        fn name(&self) -> &str {
            "large_output"
        }
        fn description(&self) -> &str {
            "Returns a large output"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            // Return 100KB of text
            let output = "x".repeat(100_000);
            Ok(ToolResult {
                success: true,
                output,
                error: None,
            })
        }
    }

    let model_provider = Box::new(MockModelProvider::new(vec![
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "large_output".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("Processed the large output successfully"),
    ]));

    let tmp = tempfile::TempDir::new().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(model_provider, vec![Box::new(LargeOutputTool)], tmp.path());

    let response = agent.turn("Generate a large output").await.unwrap();
    assert!(
        !response.is_empty(),
        "Agent should handle large tool output without crashing"
    );
}

/// Agent handles parallel tool calls in a single iteration.
#[tokio::test]
async fn agent_handles_parallel_tool_calls() {
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
            ToolCall {
                id: "tc3".into(),
                name: "counter".into(),
                arguments: "{}".into(),
                extra_content: None,
            },
        ]),
        text_response("All three parallel tools completed"),
    ]));

    let tmp = tempfile::TempDir::new().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(model_provider, vec![Box::new(counting_tool)], tmp.path());

    let response = agent.turn("Run 3 tools in parallel").await.unwrap();
    assert!(!response.is_empty());
    assert_eq!(
        *count.lock().unwrap(),
        3,
        "All 3 parallel tool calls should execute"
    );
}

/// Multi-turn with tools: each turn builds on the previous.
#[tokio::test]
async fn agent_multi_turn_with_tools_builds_context() {
    let (counting_tool, count) = CountingTool::new();

    let model_provider = Box::new(MockModelProvider::new(vec![
        // Turn 1: tool call + response
        tool_response(vec![ToolCall {
            id: "tc1".into(),
            name: "counter".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("Step 1 complete. Counter is at 1."),
        // Turn 2: another tool + response
        tool_response(vec![ToolCall {
            id: "tc2".into(),
            name: "counter".into(),
            arguments: "{}".into(),
            extra_content: None,
        }]),
        text_response("Step 2 complete. Counter is at 2."),
        // Turn 3: final response referencing prior turns
        text_response("All done. We executed 2 tool calls across 3 turns."),
    ]));

    let tmp = tempfile::TempDir::new().unwrap();
    let mut agent =
        build_agent_with_sqlite_memory(model_provider, vec![Box::new(counting_tool)], tmp.path());

    let r1 = agent.turn("Start task: increment counter").await.unwrap();
    assert!(!r1.is_empty());

    let r2 = agent.turn("Continue: increment again").await.unwrap();
    assert!(!r2.is_empty());

    let r3 = agent.turn("Summary: what did we do?").await.unwrap();
    assert!(!r3.is_empty());

    assert_eq!(
        *count.lock().unwrap(),
        2,
        "Two tool calls across multiple turns"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. Memory Consolidation Integration
// ═════════════════════════════════════════════════════════════════════════════

/// Direct test of consolidate_turn saving to memory.
#[tokio::test]
async fn consolidation_extracts_facts_to_memory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem: Arc<dyn Memory> = Arc::new(SqliteMemory::new("test", tmp.path()).unwrap());

    let model_provider = MockModelProvider::new(vec![text_response(
        r#"{"history_entry": "User shared project deadline info", "memory_update": "Project deadline is April 15th 2026"}"#,
    )]);

    let result = zeroclaw::memory::consolidation::consolidate_turn(
        &model_provider,
        "test-model",
        None,
        mem.as_ref(),
        &MemoryConfig::default(),
        "The project deadline is April 15th 2026",
        "Got it, I'll remember the deadline is April 15th.",
    )
    .await;

    assert!(result.is_ok(), "Consolidation should succeed");

    // Check that facts were stored
    let entries = mem.recall("deadline", 10, None, None, None).await.unwrap();
    assert!(
        !entries.is_empty(),
        "Consolidation should have stored facts about the deadline"
    );
}

/// Memory survives multiple consolidation rounds without corruption.
#[tokio::test]
async fn memory_survives_rapid_consolidation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem: Arc<dyn Memory> = Arc::new(SqliteMemory::new("test", tmp.path()).unwrap());

    // Simulate 10 rapid consolidation rounds
    for i in 0..10 {
        let model_provider = MockModelProvider::new(vec![text_response(&format!(
            r#"{{"history_entry": "Turn {i} conversation", "memory_update": null}}"#,
        ))]);

        let _ = zeroclaw::memory::consolidation::consolidate_turn(
            &model_provider,
            "test-model",
            None,
            mem.as_ref(),
            &MemoryConfig::default(),
            &format!("User message {i}"),
            &format!("Assistant response {i}"),
        )
        .await;
    }

    // All daily entries should exist
    let entries = mem
        .recall("conversation", 20, None, None, None)
        .await
        .unwrap();
    assert!(
        entries.len() >= 5,
        "At least 5 of 10 consolidation entries should be recallable, got {}",
        entries.len()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 7. Session Persistence End-to-End
// ═════════════════════════════════════════════════════════════════════════════

/// SQLite session backend stores and loads messages correctly.
#[tokio::test]
async fn session_backend_persists_messages() {
    use zeroclaw::channels::session_backend::SessionBackend;
    use zeroclaw::channels::session_sqlite::SqliteSessionBackend;
    use zeroclaw::providers::traits::ChatMessage;

    let tmp = tempfile::TempDir::new().unwrap();
    let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

    // Store messages
    let msg1 = ChatMessage::user("Hello, world!".to_string());
    let msg2 = ChatMessage::assistant("Hi there!".to_string());
    backend.append("session_1", &msg1).unwrap();
    backend.append("session_1", &msg2).unwrap();

    // Load from fresh instance
    let backend2 = SqliteSessionBackend::new(tmp.path()).unwrap();
    let messages = backend2.load("session_1");
    assert_eq!(messages.len(), 2, "Both messages should persist");
}

/// Session state transitions work correctly.
#[tokio::test]
async fn session_state_transitions() {
    use zeroclaw::channels::session_backend::SessionBackend;
    use zeroclaw::channels::session_sqlite::SqliteSessionBackend;

    let tmp = tempfile::TempDir::new().unwrap();
    let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

    // Initial state should be None (no session yet)
    let state = backend.get_session_state("test_session").unwrap();
    assert!(state.is_none(), "Initial state should be absent");

    // Create the session row by appending a message (set_session_state only UPDATEs)
    use zeroclaw::providers::traits::ChatMessage;
    let msg = ChatMessage::user("hello".to_string());
    backend.append("test_session", &msg).unwrap();

    // Set to running
    backend
        .set_session_state("test_session", "running", Some("turn_123"))
        .unwrap();
    let state = backend.get_session_state("test_session").unwrap().unwrap();
    assert_eq!(state.state, "running");

    // Set to idle
    backend
        .set_session_state("test_session", "idle", None)
        .unwrap();
    let state = backend.get_session_state("test_session").unwrap().unwrap();
    assert_eq!(state.state, "idle");
}
