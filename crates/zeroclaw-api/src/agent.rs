/// Streaming events emitted during an agent turn.
///
/// Used by the gateway WebSocket handler to relay real-time updates to clients.
/// Consumers that pattern-match on [`TurnEvent::ToolCall`] or
/// [`TurnEvent::ToolResult`] should preserve the stable `id` field for
/// call/result correlation.
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// A text chunk from the LLM response (may arrive many times).
    Chunk { delta: String },
    /// A reasoning/thinking chunk from a thinking model (may arrive many times).
    Thinking { delta: String },
    /// The agent is invoking a tool.
    ToolCall {
        /// Stable correlation ID shared with the matching [`TurnEvent::ToolResult`].
        id: String,
        name: String,
        args: serde_json::Value,
    },
    /// A tool has returned a result.
    ToolResult {
        /// Stable correlation ID shared with the originating [`TurnEvent::ToolCall`].
        id: String,
        name: String,
        output: String,
    },
}
