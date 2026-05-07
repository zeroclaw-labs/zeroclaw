use crate::cost::CostTracker;
use crate::cost::types::{BudgetCheck, TokenUsage as CostTokenUsage};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

// ── Cost tracking via task-local ──

/// Per-provider pricing snapshot consumed by the cost tracker.
///
/// Outer key: model provider alias (e.g. `openrouter`, `anthropic`,
/// `azure-openai`). Inner key: user-defined model identifier, optionally
/// suffixed with `.input` / `.output` to encode pricing dimension. Values
/// are USD per 1M tokens.
pub type ModelProviderPricing = HashMap<String, HashMap<String, f64>>;

/// Context for cost tracking within the tool call loop.
/// Scoped via `tokio::task_local!` at call sites (channels, gateway).
#[derive(Clone)]
pub struct ToolLoopCostTrackingContext {
    pub tracker: Arc<CostTracker>,
    pub model_provider_pricing: Arc<ModelProviderPricing>,
}

impl ToolLoopCostTrackingContext {
    pub fn new(
        tracker: Arc<CostTracker>,
        model_provider_pricing: Arc<ModelProviderPricing>,
    ) -> Self {
        Self {
            tracker,
            model_provider_pricing,
        }
    }
}

tokio::task_local! {
    pub static TOOL_LOOP_COST_TRACKING_CONTEXT: Option<ToolLoopCostTrackingContext>;
}

/// Resolve `(input, output)` per-1M-token rates for a given model on a given
/// model provider's pricing map. Lookup order:
///
/// 1. Dimension-specific keys: `{model}.input` / `{model}.output`.
/// 2. Bare model key as a flat fallback applied to whichever dimension
///    didn't match in step 1.
/// 3. The model alias path's last segment (`.../suffix`) tried under the
///    same rules.
///
/// Returns `(0.0, 0.0)` if no entry matches; the caller logs a one-shot
/// debug message in that case.
fn resolve_rates(pricing: &HashMap<String, f64>, model: &str) -> (f64, f64) {
    let try_lookup = |key: &str| -> Option<(Option<f64>, Option<f64>)> {
        let input = pricing.get(&format!("{key}.input")).copied();
        let output = pricing.get(&format!("{key}.output")).copied();
        let flat = pricing.get(key).copied();
        if input.is_none() && output.is_none() && flat.is_none() {
            None
        } else {
            Some((input.or(flat), output.or(flat)))
        }
    };

    if let Some((input, output)) = try_lookup(model) {
        return (input.unwrap_or(0.0), output.unwrap_or(0.0));
    }
    if let Some((_, suffix)) = model.rsplit_once('/')
        && let Some((input, output)) = try_lookup(suffix)
    {
        return (input.unwrap_or(0.0), output.unwrap_or(0.0));
    }
    (0.0, 0.0)
}

/// Record token usage from an LLM response via the task-local cost tracker.
/// Returns `(total_tokens, cost_usd)` on success, `None` when not scoped or no usage.
pub fn record_tool_loop_cost_usage(
    model_provider_name: &str,
    model: &str,
    usage: &zeroclaw_providers::traits::TokenUsage,
) -> Option<(u64, f64)> {
    let input_tokens = usage.input_tokens.unwrap_or(0);
    let output_tokens = usage.output_tokens.unwrap_or(0);
    let total_tokens = input_tokens.saturating_add(output_tokens);
    if total_tokens == 0 {
        return None;
    }

    let ctx = TOOL_LOOP_COST_TRACKING_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()?;

    let pricing = ctx.model_provider_pricing.get(model_provider_name);
    let (input_rate, output_rate) = pricing
        .map(|map| resolve_rates(map, model))
        .unwrap_or((0.0, 0.0));

    let cost_usage =
        CostTokenUsage::new(model, input_tokens, output_tokens, input_rate, output_rate);

    // Promote first sighting of (model_provider, model) without pricing to a WARN
    // so operators notice the silent zero-cost record before they need to
    // grep DEBUG logs. Subsequent sightings stay at DEBUG so the warn
    // stream doesn't get spammy. Missing pricing means either the
    // model_provider has no pricing map at all, or the map exists but
    // produced zero rates for this model.
    if pricing.is_none() || (input_rate == 0.0 && output_rate == 0.0) {
        warn_once_missing_pricing(model_provider_name, model);
    }

    if let Err(error) = ctx.tracker.record_usage(cost_usage.clone()) {
        tracing::warn!(
            model_provider = model_provider_name,
            model,
            "Failed to record cost tracking usage: {error}"
        );
    }

    Some((cost_usage.total_tokens, cost_usage.cost_usd))
}

/// Insert `(model_provider, model)` into `seen`. Returns `true` on first sighting,
/// `false` thereafter. Split out from `warn_once_missing_pricing` so the
/// dedup contract can be unit-tested with a caller-owned set instead of the
/// process-static one.
fn missing_pricing_first_sighting(
    seen: &Mutex<HashSet<(String, String)>>,
    model_provider: &str,
    model: &str,
) -> bool {
    seen.lock()
        .insert((model_provider.to_string(), model.to_string()))
}

/// First-time WARN, subsequent DEBUG, per `(model_provider, model)` pair.
///
/// The default pricing catalog has no entries for most non-OpenAI/Anthropic/
/// Google models. Operators only realize their cost-tracking surface is
/// reporting zero when they happen to enable DEBUG logging — a pure-DEBUG
/// signal is too quiet for "your cost enforcement is silently inert" to
/// register. Promote the first sighting per-pair to WARN with a config-path
/// pointer; all subsequent same-pair occurrences stay at DEBUG so the warn
/// stream doesn't get spammy.
fn warn_once_missing_pricing(model_provider: &str, model: &str) {
    static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if missing_pricing_first_sighting(seen, model_provider, model) {
        tracing::warn!(
            model_provider,
            model,
            "Cost tracking: no pricing entry found for {model_provider}/{model} — \
             token usage will be recorded with zero cost and budget enforcement \
             is inert for this model. Add a `pricing` table to the model provider \
             entry in config.toml (under `[providers.models.\"{model_provider}\"]`) \
             with `\"{model}.input\"` and `\"{model}.output\"` keys (USD per 1M tokens). \
             This warning fires once per (model_provider, model) pair per process.",
        );
    } else {
        tracing::debug!(
            model_provider,
            model,
            "Cost tracking recorded token usage with zero pricing (no pricing entry found)",
        );
    }
}

/// Check budget before an LLM call. Returns `None` when no cost tracking
/// context is scoped (tests, delegate, CLI without cost config).
pub fn check_tool_loop_budget() -> Option<BudgetCheck> {
    TOOL_LOOP_COST_TRACKING_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .map(|ctx| {
            ctx.tracker
                .check_budget(0.0)
                .unwrap_or(BudgetCheck::Allowed)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_seen() -> Mutex<HashSet<(String, String)>> {
        Mutex::new(HashSet::new())
    }

    #[test]
    fn first_sighting_returns_true() {
        let seen = fresh_seen();
        assert!(
            missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M2.7"),
            "first observation of a (model_provider, model) pair must report first-sighting"
        );
    }

    #[test]
    fn second_sighting_same_pair_returns_false() {
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "minimax",
            "MiniMax-M2.7"
        ));
        assert!(
            !missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M2.7"),
            "second sighting of the same pair must NOT re-fire WARN"
        );
    }

    #[test]
    fn different_models_under_same_provider_are_independent() {
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "minimax",
            "MiniMax-M2.7"
        ));
        assert!(
            missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M3.0"),
            "different model under same model_provider is a distinct pair"
        );
    }

    #[test]
    fn different_providers_for_same_model_are_independent() {
        // Same model name served by two different model_providers — operator may
        // configure them at different rates, so the warn must fire for each.
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "openrouter",
            "anthropic/claude-sonnet-4-5"
        ));
        assert!(
            missing_pricing_first_sighting(&seen, "anthropic", "anthropic/claude-sonnet-4-5"),
            "different model_provider for the same model is a distinct pair"
        );
    }

    #[test]
    fn empty_strings_dedup_independently() {
        // Defensive: empty model_provider or model shouldn't collide with each other.
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(&seen, "", "model"));
        assert!(missing_pricing_first_sighting(&seen, "model_provider", ""));
        assert!(missing_pricing_first_sighting(&seen, "", ""));
        assert!(!missing_pricing_first_sighting(&seen, "", ""));
    }
}
