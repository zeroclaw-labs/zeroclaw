use crate::cost::CostTracker;
use crate::cost::types::{BudgetCheck, TokenUsage as CostTokenUsage};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use zeroclaw_config::schema::ModelPricing;

// ── Cost tracking via task-local ──

/// Context for cost tracking within the tool call loop.
/// Scoped via `tokio::task_local!` at call sites (channels, gateway).
#[derive(Clone)]
pub struct ToolLoopCostTrackingContext {
    pub tracker: Arc<CostTracker>,
    pub prices: Arc<std::collections::HashMap<String, ModelPricing>>,
}

impl ToolLoopCostTrackingContext {
    pub fn new(
        tracker: Arc<CostTracker>,
        prices: Arc<std::collections::HashMap<String, ModelPricing>>,
    ) -> Self {
        Self { tracker, prices }
    }
}

tokio::task_local! {
    pub static TOOL_LOOP_COST_TRACKING_CONTEXT: Option<ToolLoopCostTrackingContext>;
}

/// 3-tier model pricing lookup. Order matters and is the contract:
/// 1. `<provider>/<model>` — most specific. Per-provider pricing
///    (`[providers.models.<id>].pricing`) lands here via `combined_pricing`,
///    so an operator who configures a per-provider override wins over a
///    same-named bare-model entry. This order is what makes the
///    disambiguation case work: two providers serving the same model name
///    at different rates.
/// 2. `<model>` (bare) — fallback for top-level `[cost.prices.<model>]`
///    catalogs that don't qualify by provider.
/// 3. Suffix after the last `/` — fallback for slash-bearing model IDs
///    (e.g. OpenRouter's `anthropic/claude-sonnet-4-5` resolving against
///    a bare `claude-sonnet-4-5` entry).
fn lookup_pricing<'a>(
    prices: &'a std::collections::HashMap<String, ModelPricing>,
    provider_name: &str,
    model: &str,
) -> Option<&'a ModelPricing> {
    prices
        .get(&format!("{provider_name}/{model}"))
        .or_else(|| prices.get(model))
        .or_else(|| {
            model
                .rsplit_once('/')
                .and_then(|(_, suffix)| prices.get(suffix))
        })
}

/// Record token usage from an LLM response via the task-local cost tracker.
/// Returns `(total_tokens, cost_usd)` on success, `None` when not scoped or no usage.
pub fn record_tool_loop_cost_usage(
    provider_name: &str,
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
    let pricing = lookup_pricing(&ctx.prices, provider_name, model);
    let cost_usage = CostTokenUsage::new(
        model,
        input_tokens,
        output_tokens,
        pricing.map_or(0.0, |entry| entry.input),
        pricing.map_or(0.0, |entry| entry.output),
    );

    if pricing.is_none() {
        warn_once_missing_pricing(provider_name, model);
    }

    if let Err(error) = ctx.tracker.record_usage(cost_usage.clone()) {
        tracing::warn!(
            provider = provider_name,
            model,
            "Failed to record cost tracking usage: {error}"
        );
    }

    Some((cost_usage.total_tokens, cost_usage.cost_usd))
}

/// Insert `(provider, model)` into `seen`. Returns `true` on first sighting,
/// `false` thereafter. Split out from `warn_once_missing_pricing` so the
/// dedup contract can be unit-tested with a caller-owned set instead of the
/// process-static one.
fn missing_pricing_first_sighting(
    seen: &Mutex<HashSet<(String, String)>>,
    provider: &str,
    model: &str,
) -> bool {
    seen.lock()
        .insert((provider.to_string(), model.to_string()))
}

/// First-time WARN, subsequent DEBUG, per `(provider, model)` pair.
///
/// The default pricing catalog has no entries for most non-OpenAI/Anthropic/
/// Google models. Operators only realize their cost-tracking surface is
/// reporting zero when they happen to enable DEBUG logging — a pure-DEBUG
/// signal is too quiet for "your cost enforcement is silently inert" to
/// register. Promote the first sighting per-pair to WARN with a config-path
/// pointer; all subsequent same-pair occurrences stay at DEBUG so the warn
/// stream doesn't get spammy.
fn warn_once_missing_pricing(provider: &str, model: &str) {
    static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if missing_pricing_first_sighting(seen, provider, model) {
        tracing::warn!(
            provider,
            model,
            "Cost tracking: no pricing entry found for {provider}/{model} — \
             token usage will be recorded with zero cost and budget enforcement \
             is inert for this model. Add a pricing entry to config.toml under \
             `[cost.prices.\"{provider}/{model}\"]` with `input = <USD per 1M tokens>` \
             and `output = <USD per 1M tokens>` to enable cost tracking. \
             This warning fires once per (provider, model) pair per process.",
        );
    } else {
        tracing::debug!(
            provider,
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
    use std::collections::HashMap;

    fn fresh_seen() -> Mutex<HashSet<(String, String)>> {
        Mutex::new(HashSet::new())
    }

    fn pricing(input: f64, output: f64) -> ModelPricing {
        ModelPricing { input, output }
    }

    // ── lookup_pricing: 3-tier resolution order (#6251) ──────────────

    #[test]
    fn lookup_pricing_provider_qualified_beats_bare_model() {
        // Operator has a generic top-level price for "gpt-4o" AND a
        // per-provider override on "openai/gpt-4o" (e.g. they bill OpenAI
        // direct at one rate and a proxy provider at another). The
        // qualified entry must win — that is the entire point of the
        // disambiguation feature added in #6251.
        let mut prices = HashMap::new();
        prices.insert("gpt-4o".to_string(), pricing(2.5, 10.0));
        prices.insert("openai/gpt-4o".to_string(), pricing(1.5, 6.0));

        let entry = lookup_pricing(&prices, "openai", "gpt-4o").expect("qualified entry");
        assert_eq!(
            entry.input, 1.5,
            "per-provider <provider>/<model> entry must win over bare <model>"
        );
        assert_eq!(entry.output, 6.0);
    }

    #[test]
    fn lookup_pricing_bare_model_fallback_when_no_qualified_entry() {
        // No per-provider entry — the bare top-level catalog must still
        // resolve. This is the legacy operator who has only set
        // `[cost.prices.gpt-4o]` and has not adopted per-provider pricing.
        let mut prices = HashMap::new();
        prices.insert("gpt-4o".to_string(), pricing(2.5, 10.0));

        let entry = lookup_pricing(&prices, "openai", "gpt-4o").expect("bare entry");
        assert_eq!(entry.input, 2.5);
        assert_eq!(entry.output, 10.0);
    }

    #[test]
    fn lookup_pricing_suffix_fallback_for_slash_bearing_model_ids() {
        // OpenRouter-style model IDs like "anthropic/claude-sonnet-4-5":
        // the qualified key would be "openrouter/anthropic/claude-..."
        // (highly unusual to configure), and the bare key is the model
        // string itself. If neither is configured, the suffix-after-last-/
        // fallback resolves against `claude-sonnet-4-5`. This preserves
        // the legacy behaviour for operators using OpenRouter aliases.
        let mut prices = HashMap::new();
        prices.insert("claude-sonnet-4-5".to_string(), pricing(3.0, 15.0));

        let entry =
            lookup_pricing(&prices, "openrouter", "anthropic/claude-sonnet-4-5").expect("suffix");
        assert_eq!(entry.input, 3.0);
        assert_eq!(entry.output, 15.0);
    }

    #[test]
    fn lookup_pricing_returns_none_when_no_tier_matches() {
        let prices: HashMap<String, ModelPricing> = HashMap::new();
        assert!(lookup_pricing(&prices, "openai", "gpt-4o").is_none());
    }

    #[test]
    fn lookup_pricing_disambiguates_two_providers_serving_same_model() {
        // The motivating case for #6251: the same model name "gpt-4o"
        // served by "openai" and by "azure" at different per-provider
        // rates. The merged pricing map carries both qualified entries;
        // the lookup must route each provider to its own price.
        let mut prices = HashMap::new();
        prices.insert("openai/gpt-4o".to_string(), pricing(2.5, 10.0));
        prices.insert("azure/gpt-4o".to_string(), pricing(1.0, 4.0));

        let openai_entry = lookup_pricing(&prices, "openai", "gpt-4o").expect("openai entry");
        assert_eq!(openai_entry.input, 2.5);
        let azure_entry = lookup_pricing(&prices, "azure", "gpt-4o").expect("azure entry");
        assert_eq!(azure_entry.input, 1.0);
    }

    #[test]
    fn first_sighting_returns_true() {
        let seen = fresh_seen();
        assert!(
            missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M2.7"),
            "first observation of a (provider, model) pair must report first-sighting"
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
            "different model under same provider is a distinct pair"
        );
    }

    #[test]
    fn different_providers_for_same_model_are_independent() {
        // Same model name served by two different providers — operator may
        // configure them at different rates, so the warn must fire for each.
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "openrouter",
            "anthropic/claude-sonnet-4-5"
        ));
        assert!(
            missing_pricing_first_sighting(&seen, "anthropic", "anthropic/claude-sonnet-4-5"),
            "different provider for the same model is a distinct pair"
        );
    }

    #[test]
    fn empty_strings_dedup_independently() {
        // Defensive: empty provider or model shouldn't collide with each other.
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(&seen, "", "model"));
        assert!(missing_pricing_first_sighting(&seen, "provider", ""));
        assert!(missing_pricing_first_sighting(&seen, "", ""));
        assert!(!missing_pricing_first_sighting(&seen, "", ""));
    }
}
