use std::time::Duration;

/// Streaming events emitted during an agent turn.
///
/// Used by the gateway WebSocket handler to relay real-time updates to clients.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// A text chunk from the LLM response (may arrive many times).
    Chunk { delta: String },
    /// A reasoning/thinking chunk from a thinking model (may arrive many times).
    Thinking { delta: String },
    /// The agent is invoking a tool.
    ToolCall {
        name: String,
        args: serde_json::Value,
    },
    /// A tool has returned a result.
    ToolResult { name: String, output: String },
}

/// Outcome of a single agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Success,
    Failure,
    Interrupted,
}

/// Record of a single tool call within a turn.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub arguments: serde_json::Value,
    pub result: String,
    pub success: bool,
    pub duration: Duration,
}

/// Aggregated metadata for a completed agent turn.
///
/// Produced by the agent loop after all tool calls resolve and
/// the final response is delivered. Consumed by post-turn hooks
/// (skill autogen, dialectic reasoning, etc.).
#[derive(Debug, Clone)]
pub struct TurnResult {
    pub user_message: String,
    pub tool_calls: Vec<ToolCallRecord>,
    pub tool_call_count: usize,
    pub active_skill: Option<String>,
    pub outcome: TurnOutcome,
    pub final_response: String,
    pub turn_number: u64,
}
