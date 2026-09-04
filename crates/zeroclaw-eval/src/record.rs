//! The artifact produced by running one eval case — what graders score.

use zeroclaw_api::model_provider::ConversationMessage;

/// Everything captured from a single agent run, ready for grading.
#[derive(Debug)]
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
    /// Wall-clock duration of the turns loop, in milliseconds.
    pub duration_ms: u64,
    /// Number of LLM responses observed during the run.
    pub llm_calls: u32,
}

/// Convert elapsed wall time to the report's millisecond representation.
/// Extremely long durations saturate instead of truncating through an integer cast.
pub fn duration_millis_saturating(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::duration_millis_saturating;
    use std::time::Duration;

    #[test]
    fn duration_millis_saturates_at_u64_max() {
        assert_eq!(duration_millis_saturating(Duration::MAX), u64::MAX);
    }
}
