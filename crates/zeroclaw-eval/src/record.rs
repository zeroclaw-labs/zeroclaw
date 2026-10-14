//! The artifact produced by running one eval case — what graders score.

use zeroclaw_api::model_provider::ConversationMessage;

use crate::observer::RecordedCall;

/// Everything captured from a single agent run, ready for grading.
pub struct RunRecord {
    /// The agent's final text response for the case.
    pub final_response: String,
    /// The full conversation trajectory (messages + tool calls + tool results).
    pub history: Vec<ConversationMessage>,
    /// Every dispatched tool call with its arguments and result, in call order.
    /// This creates the canonical dispatch fact: names, success, arguments, and
    /// results are derived from this list rather than copied into parallel fields.
    pub tool_calls: Vec<RecordedCall>,
    /// Accumulated input tokens reported by the provider.
    pub input_tokens: u64,
    /// Accumulated output tokens reported by the provider.
    pub output_tokens: u64,
}

impl RunRecord {
    /// Names of tools actually dispatched, in call order.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tool_calls
            .iter()
            .map(|call| call.name.as_str())
            .collect()
    }

    /// Whether every dispatched tool call succeeded (vacuously true if none).
    pub fn all_tools_succeeded(&self) -> bool {
        self.tool_calls.iter().all(|call| call.success)
    }
}
