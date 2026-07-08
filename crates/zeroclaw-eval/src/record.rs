//! The artifact produced by running one eval case — what graders score.

use zeroclaw_api::model_provider::ConversationMessage;

/// Everything captured from a single agent run, ready for grading.
pub struct RunRecord {
    /// The agent's final text response for the case.
    pub final_response: String,
    /// The full conversation trajectory (messages + tool calls + tool results).
    pub history: Vec<ConversationMessage>,
    /// Names of tools that were dispatched, in call order.
    pub tools_called: Vec<String>,
    /// Whether every dispatched tool call succeeded.
    pub all_tools_succeeded: bool,
    /// Accumulated input tokens reported by the provider.
    pub input_tokens: u64,
    /// Accumulated output tokens reported by the provider.
    pub output_tokens: u64,
}
