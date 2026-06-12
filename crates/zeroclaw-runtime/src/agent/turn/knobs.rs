//! Per-caller loop behaviour knobs (#7415 consolidation).
//!
//! Every divergence between the historical turn engines that survives the
//! consolidation is an explicit field here, set per caller. `Default`
//! preserves today's channel/CLI behaviour.

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
}

impl Default for LoopKnobs {
    fn default() -> Self {
        Self {
            dedup_enabled: true,
            max_iteration_behavior: MaxIterationBehavior::GracefulSummary,
        }
    }
}
