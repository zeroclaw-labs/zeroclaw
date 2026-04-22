pub mod tracker;
pub mod types;

// Re-exported for potential external use (public API)
#[allow(unused_imports)]
pub use tracker::CostTracker;
#[allow(unused_imports)]
pub use types::{BudgetCheck, CostRecord, CostSummary, ModelStats, TokenUsage, UsagePeriod};

use crate::config::schema::{CostConfig, ModelPricing};
use std::path::Path;
use std::sync::Arc;

/// Build an `Arc<CostTracker>` from config, honoring `enabled` and logging
/// construction failures as warnings.
///
/// Returns `None` when cost tracking is disabled or initialization fails;
/// callers can then treat absence as "cost tracking unavailable" without
/// having to replicate the enabled check + error logging pattern.
pub fn try_build_tracker(config: &CostConfig, workspace_dir: &Path) -> Option<Arc<CostTracker>> {
    if !config.enabled {
        return None;
    }
    match CostTracker::new(config.clone(), workspace_dir) {
        Ok(ct) => Some(Arc::new(ct)),
        Err(e) => {
            tracing::warn!("Failed to initialize cost tracker: {e}");
            None
        }
    }
}

/// Look up per-1M-token pricing for a `(provider, model)` pair in the cost
/// config's `prices` table.
///
/// Key format is `"<provider>/<model>"`. When the exact key is missing, the
/// provider name is first normalized via `canonical_china_provider_name`
/// (so regional aliases like `qwen-code` / `dashscope-intl` share a single
/// canonical `qwen/...` price entry) before a second lookup.
///
/// Returns `None` when no match is found. Callers MUST treat this as
/// "unknown pricing — skip recording" rather than silently recording a
/// zero-cost line; a silent zero would leave the daily/monthly budget
/// enforcement inaccurate without any visible signal.
pub fn lookup_price(config: &CostConfig, provider_name: &str, model: &str) -> Option<ModelPricing> {
    let exact_key = format!("{provider_name}/{model}");
    if let Some(p) = config.prices.get(&exact_key) {
        return Some(p.clone());
    }
    if let Some(canon) = crate::providers::canonical_china_provider_name(provider_name) {
        let canon_key = format!("{canon}/{model}");
        if let Some(p) = config.prices.get(&canon_key) {
            return Some(p.clone());
        }
    }
    None
}

/// Record a single LLM call's token usage and return the resulting budget
/// state. Call once per `provider.chat()` response from the agent loop.
///
/// Skip paths (return `None`, no record written):
/// - `tracker` is `None` — cost tracking disabled/unavailable at the call site.
/// - `usage` is `None` or has neither input nor output tokens — provider did
///   not report usage (streaming path, transport fallback, etc.). Emits a
///   `warn` log so silent data loss is visible.
/// - No price entry matches `(provider, model)` in `[cost.prices]`. Emits an
///   `error`-level log with the exact config key the operator should add.
///
/// Recording path: builds a `types::TokenUsage` with `cost_usd` computed
/// from the looked-up price, persists it, then calls `check_budget(0.0)` to
/// return the post-record budget state so callers can gate further work.
pub fn record_llm_call(
    tracker: Option<&Arc<CostTracker>>,
    provider_name: &str,
    model: &str,
    usage: Option<&crate::providers::traits::TokenUsage>,
) -> Option<BudgetCheck> {
    let tracker = tracker?;

    let usage = match usage {
        Some(u) if u.input_tokens.is_some() || u.output_tokens.is_some() => u,
        _ => {
            tracing::warn!(
                target: "cost",
                provider = provider_name,
                model = model,
                "Provider returned no token usage for this call; cost NOT recorded. \
                 Budget enforcement will underestimate usage on this turn."
            );
            return None;
        }
    };

    let price = match lookup_price(tracker.cost_config(), provider_name, model) {
        Some(p) => p,
        None => {
            tracing::error!(
                target: "cost",
                provider = provider_name,
                model = model,
                "No [cost.prices.\"{provider_name}/{model}\"] entry configured; \
                 cost for this call NOT recorded. Add the pricing entry to config.toml \
                 (input/output rates in USD per 1M tokens) to enable budget enforcement."
            );
            return None;
        }
    };

    let in_tok = usage.input_tokens.unwrap_or(0);
    let out_tok = usage.output_tokens.unwrap_or(0);
    let model_key = format!("{provider_name}/{model}");
    let token_usage = types::TokenUsage::new(model_key, in_tok, out_tok, price.input, price.output);

    if let Err(e) = tracker.record_usage(token_usage) {
        tracing::warn!(target: "cost", error = %e, "Failed to persist cost record");
        return None;
    }

    match tracker.check_budget(0.0) {
        Ok(check) => Some(check),
        Err(e) => {
            tracing::warn!(target: "cost", error = %e, "Failed to re-check budget after record");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::TokenUsage as ProviderTokenUsage;
    use tempfile::TempDir;

    fn price(input: f64, output: f64) -> ModelPricing {
        ModelPricing { input, output }
    }

    fn cost_config_with_prices(pairs: &[(&str, ModelPricing)]) -> CostConfig {
        let mut cfg = CostConfig {
            enabled: true,
            ..Default::default()
        };
        cfg.prices.clear();
        for (k, v) in pairs {
            cfg.prices.insert((*k).to_string(), v.clone());
        }
        cfg
    }

    #[test]
    fn lookup_price_hits_exact_key() {
        let cfg = cost_config_with_prices(&[("anthropic/claude-x", price(3.0, 15.0))]);
        let p = lookup_price(&cfg, "anthropic", "claude-x").expect("should hit");
        assert_eq!(p.input, 3.0);
        assert_eq!(p.output, 15.0);
    }

    #[test]
    fn lookup_price_returns_none_on_miss() {
        let cfg = cost_config_with_prices(&[("anthropic/claude-x", price(3.0, 15.0))]);
        assert!(lookup_price(&cfg, "openai", "gpt-5").is_none());
    }

    #[test]
    fn lookup_price_falls_back_to_canonical_china_alias() {
        let cfg = cost_config_with_prices(&[("qwen/qwen-plus", price(0.3, 1.8))]);
        // `qwen-code`, `dashscope-intl` canonicalize to `qwen`.
        let p = lookup_price(&cfg, "qwen-code", "qwen-plus").expect("canonical fallback");
        assert_eq!(p.input, 0.3);
        let p2 = lookup_price(&cfg, "dashscope-intl", "qwen-plus").expect("canonical fallback");
        assert_eq!(p2.output, 1.8);
    }

    #[test]
    fn record_llm_call_skips_when_tracker_none() {
        // No panic, returns None regardless of other inputs.
        let usage = ProviderTokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
        };
        assert!(record_llm_call(None, "qwen", "qwen-plus", Some(&usage)).is_none());
    }

    #[test]
    fn record_llm_call_skips_when_usage_missing() {
        let tmp = TempDir::new().unwrap();
        let cfg = cost_config_with_prices(&[("qwen/qwen-plus", price(0.3, 1.8))]);
        let tracker = Arc::new(CostTracker::new(cfg, tmp.path()).unwrap());

        let result = record_llm_call(Some(&tracker), "qwen", "qwen-plus", None);
        assert!(result.is_none());
        // No record persisted.
        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 0);
    }

    #[test]
    fn record_llm_call_skips_when_price_missing() {
        let tmp = TempDir::new().unwrap();
        // Config has ONE known price, but we'll call with a different model.
        let cfg = cost_config_with_prices(&[("qwen/qwen-plus", price(0.3, 1.8))]);
        let tracker = Arc::new(CostTracker::new(cfg, tmp.path()).unwrap());

        let usage = ProviderTokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
        };
        let result = record_llm_call(Some(&tracker), "openai", "gpt-missing-price", Some(&usage));
        assert!(result.is_none());
        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 0);
    }

    #[test]
    fn record_llm_call_records_and_returns_budget_state() {
        let tmp = TempDir::new().unwrap();
        let cfg = cost_config_with_prices(&[("qwen/qwen-plus", price(0.3, 1.8))]);
        let tracker = Arc::new(CostTracker::new(cfg, tmp.path()).unwrap());

        let usage = ProviderTokenUsage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
        };
        // 1M input * $0.3 + 1M output * $1.8 = $2.10 — well under default daily limit.
        let check = record_llm_call(Some(&tracker), "qwen", "qwen-plus", Some(&usage))
            .expect("should return a budget check");
        assert!(matches!(
            check,
            BudgetCheck::Allowed | BudgetCheck::Warning { .. }
        ));

        let summary = tracker.get_summary().unwrap();
        assert_eq!(summary.request_count, 1);
        assert!((summary.session_cost_usd - 2.10).abs() < 1e-6);
    }
}
