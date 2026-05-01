use crate::cost::CostTracker;
use crate::cost::types::{BudgetCheck, TokenUsage as CostTokenUsage};
use std::collections::HashMap;
use std::sync::Arc;

// ── Cost tracking via task-local ──

/// Per-model-provider pricing snapshot consumed by the cost tracker.
///
/// Outer key: model-provider alias (e.g. `openrouter`, `anthropic`,
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
/// model-provider's pricing map. Lookup order:
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

    if pricing.is_none() || (input_rate == 0.0 && output_rate == 0.0) {
        tracing::debug!(
            model_provider = model_provider_name,
            model,
            "Cost tracking recorded token usage with zero pricing (no pricing entry found)"
        );
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
