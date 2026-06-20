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
    /// When `true` (channel paths), a response that resembles the internal
    /// tool protocol while no tools are enabled is classified as a parse
    /// issue (malformed-protocol retry, then i18n fallback) so raw protocol
    /// text never reaches end users. Embedder wrappers set `false`: their
    /// contract is to return the model text verbatim and let the embedder
    /// do its own post-processing.
    pub detect_protocol_without_tools: bool,
    /// The agent's resolved history-pruning policy, consulted by the
    /// preemptive over-budget trim in [`super::history_window`]. Only
    /// `collapse_tool_results` and `keep_recent` are honored there;
    /// `enabled`/`max_tokens` are not — an over-budget request MUST be
    /// trimmed regardless (otherwise the provider returns
    /// `context_length_exceeded`), but when the user set `enabled = false`
    /// the override is logged rather than performed silently. `Default`
    /// (`HistoryPrunerConfig::default()`, i.e. `collapse_tool_results = true`,
    /// `keep_recent = 4`) reproduces the historical hardcoded trim behaviour
    /// for callers that build `LoopKnobs::default()`.
    pub history_pruning: crate::agent::history_pruner::HistoryPrunerConfig,
}

impl Default for LoopKnobs {
    fn default() -> Self {
        Self {
            dedup_enabled: true,
            max_iteration_behavior: MaxIterationBehavior::GracefulSummary,
            detect_protocol_without_tools: true,
            history_pruning: crate::agent::history_pruner::HistoryPrunerConfig::default(),
        }
    }
}
