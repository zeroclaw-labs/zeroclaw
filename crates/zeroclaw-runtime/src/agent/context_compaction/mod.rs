//! Context compaction by model-written summary.
//!
//! [`crate::agent::history_trim`] drops whole turns when the provider-facing
//! history exceeds the token budget. That is lossy but cheap: the dropped
//! turns are simply gone. Compaction replaces the older turns with a summary
//! the model itself writes, so the conversation keeps its meaning while the
//! request shrinks enough to fit.
//!
//! The two mechanisms compose instead of competing: compaction runs first
//! (when it is enabled and the history is over the threshold), and the
//! whole-turn trim stays the hard fallback when the summary is not enough —
//! or when it fails outright.

mod region;
mod summary;

#[cfg(test)]
mod tests;

pub(crate) use region::CompactionRegion;

use std::time::Duration;

use zeroclaw_api::model_provider::{ChatMessage, ModelProvider};
use zeroclaw_config::scattered_types::ContextCompressionConfig;

/// Result of one compaction attempt.
#[derive(Debug, Clone)]
pub(crate) struct CompactionOutcome {
    /// Whether a summary replaced older turns.
    pub(crate) compacted: bool,
    /// Whole turns replaced by the summary.
    pub(crate) turns_compacted: usize,
    /// Messages replaced by the summary.
    pub(crate) messages_compacted: usize,
    /// Estimated history tokens before the compaction.
    pub(crate) tokens_before: usize,
    /// Estimated history tokens after the compaction.
    pub(crate) tokens_after: usize,
}

impl CompactionOutcome {
    /// Nothing was compacted: the history is under the threshold, or too short
    /// to protect the head and tail turns.
    pub(crate) fn skipped(tokens: usize) -> Self {
        Self {
            compacted: false,
            turns_compacted: 0,
            messages_compacted: 0,
            tokens_before: tokens,
            tokens_after: tokens,
        }
    }
}

/// Everything one compaction pass needs.
pub(crate) struct CompactionRequest<'a> {
    /// The provider-facing history, rewritten in place when the compaction
    /// succeeds.
    pub(crate) history: &'a mut Vec<ChatMessage>,
    /// Whether the population already carries a synthetic head slot (the trim
    /// breadcrumb or a previous compaction summary). The owner owns that
    /// record; the compaction never infers it from message text.
    pub(crate) crumb_present: &'a mut bool,
    /// Provider used for the summary. The agent's own provider, or the
    /// configured summary provider.
    pub(crate) provider: &'a dyn ModelProvider,
    /// Model used for the summary.
    pub(crate) model: &'a str,
    /// Sampling temperature for the summary call.
    pub(crate) temperature: Option<f64>,
    /// Compaction knobs.
    pub(crate) config: &'a ContextCompressionConfig,
    /// Context window of the selected route, used to derive the threshold.
    pub(crate) context_window: usize,
    /// Ignore the threshold and compact whenever there is anything to
    /// compact (the manual `/compact` path).
    pub(crate) forced: bool,
    /// Hard timeout for the summary call.
    pub(crate) timeout: Duration,
}

/// Token count at which compaction kicks in.
///
/// `ratio` is a fraction of the model context window. Values outside
/// `(0.0, 1.0]` fall back to the configured default.
pub(crate) fn threshold_tokens(context_window: usize, ratio: f64) -> usize {
    if context_window == 0 {
        return usize::MAX;
    }
    let ratio = if ratio > 0.0 && ratio <= 1.0 {
        ratio
    } else {
        DEFAULT_THRESHOLD_RATIO
    };
    ((context_window as f64 * ratio) as usize).max(1)
}

/// Default threshold when the configured ratio is unusable.
pub(crate) const DEFAULT_THRESHOLD_RATIO: f64 = 0.5;

/// Effective compaction knobs for an agent.
///
/// Returns `None` when the agent has no runtime profile: without one there is
/// nothing to configure, and compaction stays off. The `enabled` flag gates
/// the AUTOMATIC path only — a manual `/compact` is explicit intent and runs
/// regardless.
pub(crate) fn effective_config<'a>(
    config: Option<&'a zeroclaw_config::schema::Config>,
    agent_alias: Option<&str>,
) -> Option<&'a ContextCompressionConfig> {
    let config = config?;
    let alias = agent_alias?;
    let profile = config.runtime_profile_for_agent(alias)?;
    Some(&profile.context_compression)
}

/// Compact the history when it is over the threshold, or unconditionally when
/// `forced` is set.
///
/// The history is only rewritten when a summary was actually produced: a
/// failed or timed-out summary leaves the history untouched, so the
/// whole-turn trim remains the safety net.
pub(crate) async fn compact_history(request: CompactionRequest<'_>) -> CompactionOutcome {
    let CompactionRequest {
        history,
        crumb_present,
        provider,
        model,
        temperature,
        config,
        context_window,
        forced,
        timeout,
    } = request;
    let tokens_before = crate::agent::history::estimate_history_tokens(history);
    let threshold = threshold_tokens(context_window, config.threshold_ratio);
    if !forced && tokens_before < threshold {
        return CompactionOutcome::skipped(tokens_before);
    }
    let Some(region) = region::select(history, *crumb_present, config) else {
        return CompactionOutcome::skipped(tokens_before);
    };
    let summary = summary::summarize(&region, provider, model, temperature, config, timeout).await;
    if summary.trim().is_empty() {
        return CompactionOutcome::skipped(tokens_before);
    }
    region::install(history, crumb_present, &region, &summary);
    let tokens_after = crate::agent::history::estimate_history_tokens(history);
    CompactionOutcome {
        compacted: true,
        turns_compacted: region.turns,
        messages_compacted: region.messages,
        tokens_before,
        tokens_after,
    }
}
