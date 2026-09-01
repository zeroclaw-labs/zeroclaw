//! Per-caller loop behaviour knobsconsolidation).

use zeroclaw_config::schema::StreamReasoningMode;

/// How to handle max-tool-iteration exhaustion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaxIterationBehavior {
    /// Ask the LLM for a tools-free final summary (channel/CLI behaviour).
    #[default]
    GracefulSummary,
    /// Bail with "exceeded maximum tool iterations" (embedder control signal).
    ErrorAtCap,
}

/// Explicit knobs for per-caller loop behaviour.
#[derive(Debug, Clone)]
pub struct LoopKnobs {
    pub dedup_enabled: bool,
    pub max_iteration_behavior: MaxIterationBehavior,
    pub detect_protocol_without_tools: bool,
    /// Controls whether provider reasoning deltas are reflected into the
    /// draft/status surface. Raw reasoning is opt-in; the default only emits a
    /// liveness tick so existing channel progress remains privacy-preserving.
    pub draft_reasoning: StreamReasoningMode,
}

impl Default for LoopKnobs {
    fn default() -> Self {
        Self {
            dedup_enabled: true,
            max_iteration_behavior: MaxIterationBehavior::GracefulSummary,
            detect_protocol_without_tools: true,
            draft_reasoning: StreamReasoningMode::Status,
        }
    }
}
