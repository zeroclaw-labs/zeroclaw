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

/// Where a turn originated — used by hooks to distinguish operator
/// interaction from automated/scheduled execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnSource {
    /// Interactive CLI session.
    Cli,
    /// Channel message (Telegram, Slack, etc.) from an operator.
    Channel,
    /// Cron scheduler — no human in the loop.
    Cron,
    /// Heartbeat task execution — autonomous, task-bound.
    Heartbeat,
}

impl TurnSource {
    pub fn is_automated(self) -> bool {
        matches!(self, Self::Cron | Self::Heartbeat)
    }
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
    pub turn_source: TurnSource,
    pub outcome: TurnOutcome,
    pub final_response: String,
    pub turn_number: u64,
}
