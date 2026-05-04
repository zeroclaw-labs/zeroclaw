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
    // 3-tier model pricing lookup: direct name → provider/model → suffix after last `/`
    let pricing = ctx
        .prices
        .get(model)
        .or_else(|| ctx.prices.get(&format!("{provider_name}/{model}")))
        .or_else(|| {
            model
                .rsplit_once('/')
                .and_then(|(_, suffix)| ctx.prices.get(suffix))
        });
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
    let key = (provider.to_string(), model.to_string());
    let first_sighting = seen.lock().insert(key);
    if first_sighting {
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
