/// Streaming events emitted during an agent turn.
///
/// Used by the gateway WebSocket handler to relay real-time updates to clients.
/// Consumers that pattern-match on [`TurnEvent::ToolCall`] or
/// [`TurnEvent::ToolResult`] should preserve the stable `id` field for
/// call/result correlation.
use crate::plan::PlanEntry;

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
        presentation: crate::tool::ToolPresentation,
    },
    /// A tool has returned a result.
    ToolResult {
        /// Stable correlation ID shared with the originating [`TurnEvent::ToolCall`].
        id: String,
        name: String,
        output: String,
    },
    /// The agent published or updated its execution plan (TodoWrite).
    ///
    /// Whole-list replacement: `entries` is the complete authoritative
    /// plan; an empty vec clears it. Downstream consumers replace their
    /// held plan wholesale — no merge.
    Plan { entries: Vec<PlanEntry> },
    /// The agent is waiting for the operator to approve, deny, or always-allow
    /// a tool call. The transport (e.g. gateway WebSocket) is expected to
    /// surface this to the operator and route the response back through the
    /// same correlation `request_id`. The runtime tool loop pauses until that
    /// answer arrives or the channel times out.
    ApprovalRequest {
        /// Correlation ID. The matching response frame must echo it.
        request_id: String,
        tool_name: String,
        presentation: crate::tool::ToolPresentation,
        /// Human-readable, secret-redacted summary of the tool arguments.
        /// Synthesised by `crate::approval::summarize_args`; never the raw
        /// `args` value.
        arguments_summary: String,
        /// How long the channel will wait before auto-denying.
        timeout_secs: u64,
    },
    /// Older whole turns were dropped from the context window to fit the token
    /// budget. Surfaces a user-visible "context was cut here" marker so trimming
    /// is never silent. Emitted once per turn boundary when a trim occurs.
    HistoryTrimmed {
        dropped_messages: usize,
        kept_turns: usize,
        reason: String,
    },
    /// Per-LLM-call token usage and cost.
    ///
    /// Emitted once per LLM response the agent loop processes; a single turn
    /// that hops through tools may emit several `Usage` events, one per model
    /// call. Consumers (e.g. the gateway WS handler) accumulate these into a
    /// turn total before reporting back to the client. Absence means "usage
    /// unavailable for this call" rather than zero.
    Usage {
        input_tokens: Option<u64>,
        /// Tokens served from the provider's prompt cache (e.g. Anthropic
        /// `cache_read_input_tokens`, OpenAI `cached_tokens`). These count
        /// toward the context window and must be added to `input_tokens` to
        /// get the true total context size.
        cached_input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cost_usd: Option<f64>,
    },
}

#[cfg(test)]
mod plan_event_tests {
    use super::*;
    use crate::plan::{PlanEntry, PlanPriority, PlanStatus};

    #[test]
    fn plan_turn_event_carries_entries() {
        let ev = TurnEvent::Plan {
            entries: vec![PlanEntry {
                content: "Step one".to_string(),
                status: PlanStatus::Pending,
                priority: PlanPriority::Medium,
                active_form: None,
            }],
        };
        match ev {
            TurnEvent::Plan { entries } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].content, "Step one");
            }
            _ => panic!("expected TurnEvent::Plan"),
        }
    }
}
