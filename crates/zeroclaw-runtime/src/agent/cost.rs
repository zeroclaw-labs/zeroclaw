use crate::cost::CostTracker;
use crate::cost::types::{BudgetCheck, TokenUsage as CostTokenUsage};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use zeroclaw_config::schema::Config;
use zeroclaw_providers::pricing::ModelRates;

// ── Cost tracking via task-local ──

pub type ModelProviderPricing = HashMap<String, HashMap<String, f64>>;

/// Per-scope token/cost accumulator derived from the usage events emitted
/// during a single task-local runtime invocation.
#[derive(Default, Clone, Copy, Debug)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub last_input_tokens: u64,
}

pub fn build_model_provider_pricing(config: &Config) -> ModelProviderPricing {
    let mut pricing: ModelProviderPricing = HashMap::new();

    for (type_k, alias_k, profile) in config.providers.models.iter_entries() {
        let mut slot = profile.pricing.clone();
        apply_rate_sheet_pricing(config, type_k, &mut slot);
        if !slot.is_empty() {
            pricing.insert(format!("{type_k}.{alias_k}"), slot);
        }
    }

    pricing
}

pub fn tool_loop_cost_tracking_context_for_agent(
    config: &Config,
    agent_alias: &str,
) -> Option<ToolLoopCostTrackingContext> {
    CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir)
        .map(|tracker| tool_loop_cost_tracking_context_from_tracker(config, agent_alias, tracker))
}

pub fn tool_loop_cost_tracking_context_from_tracker(
    config: &Config,
    agent_alias: &str,
    tracker: Arc<CostTracker>,
) -> ToolLoopCostTrackingContext {
    ToolLoopCostTrackingContext::new(tracker, Arc::new(build_model_provider_pricing(config)))
        .with_agent_alias(agent_alias)
}

pub fn build_type_level_model_provider_pricing(config: &Config) -> ModelProviderPricing {
    let mut pricing: ModelProviderPricing = HashMap::new();

    for (type_k, _alias_k, profile) in config.providers.models.iter_entries() {
        if profile.pricing.is_empty() {
            continue;
        }
        let slot = pricing.entry(type_k.to_string()).or_default();
        merge_pricing(slot, &profile.pricing);
    }

    for (provider_type, _model_id, _rates) in config.cost.rates.providers.models.iter_entries() {
        let slot = pricing.entry(provider_type.to_string()).or_default();
        apply_rate_sheet_pricing(config, provider_type, slot);
    }

    pricing
}

pub fn provider_pricing<'a>(
    pricing: &'a ModelProviderPricing,
    model_provider_name: &str,
) -> Option<&'a HashMap<String, f64>> {
    if let Some(slot) = pricing.get(model_provider_name) {
        return Some(slot);
    }

    // Type-keyed maps are still used in the channels path; when the lookup
    // arrives as `<type>.<alias>`, fall back to the bare provider family.
    if let Some((provider_type, _alias)) = model_provider_name.split_once('.')
        && let Some(slot) = pricing.get(provider_type)
    {
        return Some(slot);
    }

    // Some call sites surface only the bare provider type while the
    // pricing view is keyed by `<type>.<alias>`. Fall back only when the
    // type resolves to exactly one alias entry so pricing stays deterministic.
    let prefix = format!("{model_provider_name}.");
    let mut matches = pricing
        .iter()
        .filter(|(key, _)| key.starts_with(&prefix))
        .map(|(_, slot)| slot);
    let first = matches.next()?;
    if matches.next().is_none() {
        Some(first)
    } else {
        None
    }
}

fn apply_rate_sheet_pricing(config: &Config, provider_type: &str, slot: &mut HashMap<String, f64>) {
    for (rate_provider_type, model_id, rates) in config.cost.rates.providers.models.iter_entries() {
        if rate_provider_type != provider_type {
            continue;
        }
        if let Some(input) = rates.input_per_mtok {
            slot.insert(format!("{model_id}.input"), input);
        }
        if let Some(output) = rates.output_per_mtok {
            slot.insert(format!("{model_id}.output"), output);
        }
        if let Some(cached) = rates.cached_input_per_mtok {
            slot.insert(format!("{model_id}.cached_input"), cached);
        }
    }
}

fn merge_pricing(slot: &mut HashMap<String, f64>, pricing: &HashMap<String, f64>) {
    for (key, value) in pricing {
        slot.insert(key.clone(), *value);
    }
}

/// Context for cost tracking within the tool call loop.
/// Scoped via `tokio::task_local!` at call sites (channels, gateway).
#[derive(Clone)]
pub struct ToolLoopCostTrackingContext {
    /// Shared cost tracker. `None` for usage-only contexts that accumulate
    /// per-turn token totals without persisting cost records or enforcing
    /// budgets (see [`Self::usage_only`]).
    pub tracker: Option<Arc<CostTracker>>,
    pub model_provider_pricing: Arc<ModelProviderPricing>,
    /// Per-scope usage accumulator so wrapping code can read token/cost
    /// totals after the scoped future exits (without racing concurrent
    /// traffic sharing the same tracker).
    pub turn_usage: Arc<Mutex<TurnUsage>>,
    /// Alias of the agent driving this turn. Stamped onto persisted
    /// `CostRecord`s so `/api/cost?agent=<alias>` can attribute spend.
    pub agent_alias: Option<String>,
}

impl ToolLoopCostTrackingContext {
    pub fn new(
        tracker: Arc<CostTracker>,
        model_provider_pricing: Arc<ModelProviderPricing>,
    ) -> Self {
        Self {
            tracker: Some(tracker),
            model_provider_pricing,
            turn_usage: Arc::new(Mutex::new(TurnUsage::default())),
            agent_alias: None,
        }
    }

    pub fn usage_only() -> Self {
        Self {
            tracker: None,
            model_provider_pricing: Arc::new(ModelProviderPricing::new()),
            turn_usage: Arc::new(Mutex::new(TurnUsage::default())),
            agent_alias: None,
        }
    }

    /// Attach an agent alias to this context so subsequent
    /// `record_tool_loop_cost_usage` calls stamp records with it.
    #[must_use]
    pub fn with_agent_alias(mut self, agent_alias: impl Into<String>) -> Self {
        self.agent_alias = Some(agent_alias.into());
        self
    }

    pub fn snapshot_turn_usage(&self) -> TurnUsage {
        TOOL_LOOP_TURN_USAGE
            .try_with(|turn_usage| turn_usage.as_ref().map(|u| *u.lock()).unwrap_or_default())
            .unwrap_or_else(|_| *self.turn_usage.lock())
    }
}

tokio::task_local! {
    pub static TOOL_LOOP_COST_TRACKING_CONTEXT: Option<ToolLoopCostTrackingContext>;
}

tokio::task_local! {
    pub static TOOL_LOOP_TURN_USAGE: Option<Arc<Mutex<TurnUsage>>>;
}

fn resolve_rates_opt(pricing: &HashMap<String, f64>, model: &str) -> ModelRates {
    let try_lookup = |key: &str| -> Option<ModelRates> {
        let input = pricing.get(&format!("{key}.input")).copied();
        let output = pricing.get(&format!("{key}.output")).copied();
        let cached = pricing.get(&format!("{key}.cached_input")).copied();
        let flat = pricing.get(key).copied();
        if input.is_none() && output.is_none() && cached.is_none() && flat.is_none() {
            None
        } else {
            Some(ModelRates {
                input_per_mtok: input.or(flat),
                output_per_mtok: output.or(flat),
                cached_input_per_mtok: cached,
            })
        }
    };

    zeroclaw_providers::pricing::model_id_candidates(model)
        .find_map(try_lookup)
        .unwrap_or_default()
}

fn live_pricing_for(model_provider_name: &str, model: &str) -> Option<ModelRates> {
    let snapshot = zeroclaw_providers::pricing::current_snapshot();
    zeroclaw_providers::pricing::lookup(&snapshot, model_provider_name, model).copied()
}

/// Merge configured rates with the live-price fallback without discarding
/// whether each dimension was present. Config wins per dimension
/// ([`ModelRates::or`]); live only fills dimensions config left unset.
fn merge_config_and_live_rates(config_rates: ModelRates, live: Option<ModelRates>) -> ModelRates {
    config_rates.or(live.unwrap_or_default())
}

fn pricing_available_for_usage(
    rates: ModelRates,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
) -> bool {
    let cached_input_tokens = cached_input_tokens.min(input_tokens);
    let uncached_input_tokens = input_tokens.saturating_sub(cached_input_tokens);
    let uncached_input_priced = uncached_input_tokens == 0 || rates.input_per_mtok.is_some();
    let cached_input_priced = cached_input_tokens == 0
        || rates.cached_input_per_mtok.is_some()
        || rates.input_per_mtok.is_some();
    let output_priced = output_tokens == 0 || rates.output_per_mtok.is_some();

    uncached_input_priced && cached_input_priced && output_priced
}

/// A model with token usage whose ledger records explicitly report that one
/// or more token-bearing pricing dimensions were unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpricedModel {
    pub model: String,
    pub unpriced_tokens: u64,
}

/// Return per-model unpriced token subsets from the ledger-derived summary,
/// sorted by model id for stable output.
pub fn unpriced_models_in_summary(
    by_model: &HashMap<String, zeroclaw_config::cost::ModelStats>,
) -> Vec<UnpricedModel> {
    let mut out: Vec<UnpricedModel> = by_model
        .values()
        .filter(|m| m.unpriced_tokens > 0)
        .map(|m| UnpricedModel {
            model: m.model.clone(),
            unpriced_tokens: m.unpriced_tokens,
        })
        .collect();
    out.sort_by(|a, b| a.model.cmp(&b.model));
    out
}

/// Record usage from a rejected provider attempt without replacing the accepted
/// response's context-window fill.
pub fn record_rejected_tool_loop_cost_usage(
    model_provider_name: &str,
    model: &str,
    usage: &zeroclaw_providers::traits::TokenUsage,
) -> Option<(u64, f64)> {
    record_tool_loop_cost_usage_inner(model_provider_name, model, usage, false)
}

/// Record token usage from an accepted LLM response via the task-local cost
/// tracker. Returns `(total_tokens, cost_usd)` on success, `None` when not
/// scoped or no usage.
pub fn record_tool_loop_cost_usage(
    model_provider_name: &str,
    model: &str,
    usage: &zeroclaw_providers::traits::TokenUsage,
) -> Option<(u64, f64)> {
    record_tool_loop_cost_usage_inner(model_provider_name, model, usage, true)
}

fn record_tool_loop_cost_usage_inner(
    model_provider_name: &str,
    model: &str,
    usage: &zeroclaw_providers::traits::TokenUsage,
    updates_context_window_fill: bool,
) -> Option<(u64, f64)> {
    let input_tokens = usage.input_tokens.unwrap_or(0);
    let output_tokens = usage.output_tokens.unwrap_or(0);
    let cached_input_tokens = usage.cached_input_tokens.unwrap_or(0);
    let total_tokens = input_tokens.saturating_add(output_tokens);
    if total_tokens == 0 {
        return None;
    }

    let ctx = TOOL_LOOP_COST_TRACKING_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten()?;
    let pricing = provider_pricing(&ctx.model_provider_pricing, model_provider_name);
    let config_rates = pricing
        .map(|map| resolve_rates_opt(map, model))
        .unwrap_or_default();

    // Live-price FALLBACK fills only the dimensions config left unset; never
    // fetches on this path (reads a cached snapshot, empty unless a provider
    // opted into `live_pricing`).
    let live = (!config_rates.is_complete())
        .then(|| live_pricing_for(model_provider_name, model))
        .flatten();
    let mut rates = merge_config_and_live_rates(config_rates, live);

    if rates.input_per_mtok.is_none()
        && rates.output_per_mtok.is_none()
        && let Some((cat_in, cat_out, cat_cached)) =
            crate::agent::pricing_catalog::global_pricing_rates(model)
    {
        rates = rates.or(ModelRates {
            input_per_mtok: (cat_in > 0.0).then_some(cat_in),
            output_per_mtok: (cat_out > 0.0).then_some(cat_out),
            cached_input_per_mtok: (cat_cached > 0.0).then_some(cat_cached),
        });
    }

    let pricing_available =
        pricing_available_for_usage(rates, input_tokens, cached_input_tokens, output_tokens);
    let input_rate = rates.input_per_mtok.unwrap_or(0.0);
    let output_rate = rates.output_per_mtok.unwrap_or(0.0);
    let cached_rate = rates.cached_input_per_mtok.unwrap_or(0.0);

    let mut cost_usage = CostTokenUsage::new_with_cache(
        model,
        input_tokens,
        cached_input_tokens,
        output_tokens,
        input_rate,
        cached_rate,
        output_rate,
    );
    cost_usage.pricing_available = pricing_available;

    if ctx.tracker.is_some() && !pricing_available {
        warn_once_missing_pricing(model_provider_name, model);
    }

    // Accumulate turn usage: prefer the caller-scoped TOOL_LOOP_TURN_USAGE
    // task-local (ws.rs gateway path), fall back to the context's own
    // turn_usage field (Agent::turn_streamed path, where the task-local is
    // not set up).
    let accumulated = TOOL_LOOP_TURN_USAGE.try_with(|turn_usage| {
        if let Some(turn_usage) = turn_usage {
            let mut usage = turn_usage.lock();
            usage.input_tokens = usage.input_tokens.saturating_add(input_tokens);
            usage.output_tokens = usage.output_tokens.saturating_add(output_tokens);
            usage.cost_usd += cost_usage.cost_usd;
            if updates_context_window_fill {
                // Replace (not accumulate) last_input_tokens with the absolute
                // accepted provider-reported prompt size — this is the accurate
                // "context window fill" measure (see TurnUsage doc comment).
                usage.last_input_tokens = input_tokens;
            }
            true
        } else {
            false
        }
    });
    if !accumulated.unwrap_or(false) {
        let mut turn_usage = ctx.turn_usage.lock();
        turn_usage.input_tokens = turn_usage.input_tokens.saturating_add(input_tokens);
        turn_usage.output_tokens = turn_usage.output_tokens.saturating_add(output_tokens);
        turn_usage.cost_usd += cost_usage.cost_usd;
        if updates_context_window_fill {
            // Replace (not accumulate) last_input_tokens with the absolute
            // accepted provider-reported prompt size.
            turn_usage.last_input_tokens = input_tokens;
        }
    }

    if let Some(tracker) = &ctx.tracker
        && let Err(error) =
            tracker.record_usage_with_agent(cost_usage.clone(), ctx.agent_alias.as_deref())
    {
        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_category(::zeroclaw_log::EventCategory::Provider).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model_provider": model_provider_name, "model": model, "error": format!("{}", error)})), "Failed to record cost tracking usage: ");
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

fn warn_once_missing_pricing(model_provider: &str, model: &str) {
    static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if missing_pricing_first_sighting(seen, model_provider, model) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(
                    ::serde_json::json!({"model_provider": model_provider, "model": model})
                ),
            "Cost tracking: no pricing entry found for {model_provider}/{model} — \
             token usage will be recorded with zero cost and budget enforcement \
             is inert for this model. Add a `pricing` table to the model provider \
             entry in config.toml (under `[providers.models.\"{model_provider}\"]`) \
             with `\"{model}.input\"` and `\"{model}.output\"` keys (USD per 1M tokens). \
             This warning fires once per (model_provider, model) pair per process."
        );
    } else {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::Provider)
                .with_attrs(
                    ::serde_json::json!({"model_provider": model_provider, "model": model})
                ),
            "Cost tracking recorded token usage with zero pricing (no pricing entry found)"
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
        .and_then(|ctx| {
            ctx.tracker
                .map(|tracker| tracker.check_budget(0.0).unwrap_or(BudgetCheck::Allowed))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{Config, DeepseekModelProviderConfig, ModelProviderConfig};

    fn fresh_seen() -> Mutex<HashSet<(String, String)>> {
        Mutex::new(HashSet::new())
    }

    fn model_stats(
        model: &str,
        total_tokens: u64,
        cost_usd: f64,
        unpriced_tokens: u64,
    ) -> zeroclaw_config::cost::ModelStats {
        zeroclaw_config::cost::ModelStats {
            model: model.to_string(),
            cost_usd,
            input_tokens: total_tokens,
            cached_input_tokens: 0,
            output_tokens: 0,
            total_tokens,
            unpriced_tokens,
            request_count: 1,
        }
    }

    #[test]
    fn unpriced_models_use_recorded_provenance_not_aggregate_cost() {
        let mut by_model = HashMap::new();
        by_model.insert(
            "claude-haiku-4-5-20251001".to_string(),
            model_stats("claude-haiku-4-5-20251001", 9420, 0.0, 9420),
        );
        by_model.insert(
            "configured-free".to_string(),
            model_stats("configured-free", 1000, 0.0, 0),
        );
        by_model.insert("mixed".to_string(), model_stats("mixed", 3000, 0.05, 700));

        let unpriced = unpriced_models_in_summary(&by_model);
        assert_eq!(unpriced.len(), 2);
        assert_eq!(unpriced[0].model, "claude-haiku-4-5-20251001");
        assert_eq!(unpriced[0].unpriced_tokens, 9420);
        assert_eq!(unpriced[1].model, "mixed");
        assert_eq!(unpriced[1].unpriced_tokens, 700);
    }

    /// Output is sorted by model id for stable, deterministic status output.
    #[test]
    fn unpriced_models_are_sorted_by_id() {
        let mut by_model = HashMap::new();
        by_model.insert("zeta".to_string(), model_stats("zeta", 10, 0.0, 10));
        by_model.insert("alpha".to_string(), model_stats("alpha", 20, 0.0, 20));
        by_model.insert("mike".to_string(), model_stats("mike", 30, 0.0, 30));

        let unpriced = unpriced_models_in_summary(&by_model);
        let ids: Vec<&str> = unpriced.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "mike", "zeta"]);
    }

    #[test]
    fn genuinely_free_model_does_not_warn() {
        let mut by_model = HashMap::new();
        by_model.insert(
            "configured-free".to_string(),
            model_stats("configured-free", 1000, 0.0, 0),
        );
        assert!(unpriced_models_in_summary(&by_model).is_empty());
    }

    /// Routing Anthropic through the existing global catalog
    /// (rather than a bespoke hand-maintained table) prices a direct-provider
    /// Anthropic model by its bare model id. This is the drift-free path — the
    /// same catalog that prices every other provider.
    #[test]
    fn global_catalog_prices_anthropic_model_by_id() {
        use crate::agent::pricing_catalog::{GlobalPricingCatalog, set_global_pricing_catalog};
        let catalog: GlobalPricingCatalog = serde_json::from_str(
            r#"{"models":{"claude-haiku-4-5-20251001":{"input_usd_per_mtok":1.0,"output_usd_per_mtok":5.0,"cache_read_usd_per_mtok":0.1}}}"#,
        )
        .expect("catalog json parses");
        set_global_pricing_catalog(catalog);

        let rates =
            crate::agent::pricing_catalog::global_pricing_rates("claude-haiku-4-5-20251001");
        assert_eq!(
            rates,
            Some((1.0, 5.0, 0.1)),
            "catalog must price the direct Anthropic model by its bare id"
        );

        // Reset the process-global catalog so we don't leak into other tests.
        set_global_pricing_catalog(GlobalPricingCatalog::default());
    }

    #[test]
    fn config_rate_wins_live_fills_only_gaps() {
        // Config priced ONLY input; live prices all three. Input must stay the
        // configured value; output/cached come from live.
        let config = ModelRates {
            input_per_mtok: Some(5.0),
            ..ModelRates::default()
        };
        let live = Some(ModelRates {
            input_per_mtok: Some(99.0),
            output_per_mtok: Some(15.0),
            cached_input_per_mtok: Some(1.5),
        });
        assert_eq!(
            merge_config_and_live_rates(config, live),
            ModelRates {
                input_per_mtok: Some(5.0),
                output_per_mtok: Some(15.0),
                cached_input_per_mtok: Some(1.5),
            }
        );
    }

    #[test]
    fn no_live_leaves_unconfigured_dimensions_zero() {
        // Empty config + no live snapshot must reproduce today's behavior
        // exactly: all rates zero.
        assert_eq!(
            merge_config_and_live_rates(ModelRates::default(), None),
            ModelRates::default()
        );
        // A configured zero (genuinely free) is preserved, not "filled".
        assert_eq!(
            merge_config_and_live_rates(
                ModelRates {
                    input_per_mtok: Some(0.0),
                    output_per_mtok: Some(0.0),
                    cached_input_per_mtok: None,
                },
                Some(ModelRates {
                    input_per_mtok: Some(3.0),
                    output_per_mtok: Some(9.0),
                    cached_input_per_mtok: Some(0.3),
                })
            ),
            ModelRates {
                input_per_mtok: Some(0.0),
                output_per_mtok: Some(0.0),
                cached_input_per_mtok: Some(0.3),
            }
        );
    }

    #[test]
    fn pricing_availability_requires_each_token_bearing_dimension() {
        let input_only = ModelRates {
            input_per_mtok: Some(0.0),
            output_per_mtok: None,
            cached_input_per_mtok: None,
        };
        assert!(pricing_available_for_usage(input_only, 100, 0, 0));
        assert!(!pricing_available_for_usage(input_only, 100, 0, 1));
        assert!(pricing_available_for_usage(input_only, 100, 100, 0));

        let cached_only = ModelRates {
            input_per_mtok: None,
            output_per_mtok: None,
            cached_input_per_mtok: Some(0.0),
        };
        assert!(pricing_available_for_usage(cached_only, 100, 100, 0));
        assert!(!pricing_available_for_usage(cached_only, 100, 99, 0));
    }

    #[test]
    fn first_sighting_returns_true() {
        let seen = fresh_seen();
        assert!(
            missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M3"),
            "first observation of a (model_provider, model) pair must report first-sighting"
        );
    }

    #[test]
    fn second_sighting_same_pair_returns_false() {
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "minimax",
            "MiniMax-M3"
        ));
        assert!(
            !missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M3"),
            "second sighting of the same pair must NOT re-fire WARN"
        );
    }

    #[test]
    fn different_models_under_same_provider_are_independent() {
        let seen = fresh_seen();
        assert!(missing_pricing_first_sighting(
            &seen,
            "minimax",
            "MiniMax-M3"
        ));
        assert!(
            missing_pricing_first_sighting(&seen, "minimax", "MiniMax-M2.7"),
            "different model under same model_provider is a distinct pair"
        );
    }

    #[test]
    fn provider_pricing_resolves_composite_and_bare_type_keys() {
        let mut model_rates: HashMap<String, f64> = HashMap::new();
        model_rates.insert("glm-5.1.input".to_string(), 1.4);
        model_rates.insert("glm-5.1.output".to_string(), 4.4);

        // CLI / agent-loop builder keys by the composite `<type>.<alias>`.
        let mut composite_keyed: ModelProviderPricing = HashMap::new();
        composite_keyed.insert("glm.default".to_string(), model_rates.clone());
        assert!(
            provider_pricing(&composite_keyed, "glm.default").is_some(),
            "composite-keyed map must resolve via the verbatim composite lookup"
        );

        // Channel orchestrator builder keys by the bare provider `<type>`, yet
        // the lookup still arrives as the composite alias — must fall back.
        let mut type_keyed: ModelProviderPricing = HashMap::new();
        type_keyed.insert("glm".to_string(), model_rates.clone());
        assert!(
            provider_pricing(&type_keyed, "glm.default").is_some(),
            "type-keyed map must resolve the composite alias via the bare-type fallback"
        );

        // An unrelated provider must not accidentally match.
        assert!(
            provider_pricing(&type_keyed, "openai.default").is_none(),
            "fallback must not resolve a provider type absent from the map"
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

    fn pricing_with_cache(
        model: &str,
        input: f64,
        cached_input: f64,
        output: f64,
    ) -> HashMap<String, f64> {
        let mut map = HashMap::new();
        map.insert(format!("{model}.input"), input);
        map.insert(format!("{model}.cached_input"), cached_input);
        map.insert(format!("{model}.output"), output);
        map
    }

    #[test]
    fn build_model_provider_pricing_prefers_rate_sheet_over_legacy_alias_pricing() {
        let mut config = Config::default();
        config.providers.models.deepseek.insert(
            "default".to_string(),
            DeepseekModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("deepseek-v4-flash".into()),
                    pricing: HashMap::from([("deepseek-v4-flash.output".into(), 0.77)]),
                    ..Default::default()
                },
            },
        );
        config.cost.rates.providers.models.deepseek.insert(
            "deepseek-v4-flash".to_string(),
            zeroclaw_config::schema::ModelCostRates {
                input_per_mtok: Some(0.14),
                output_per_mtok: Some(0.28),
                cached_input_per_mtok: Some(0.0028),
            },
        );

        let alias_map = build_model_provider_pricing(&config);
        let deepseek = alias_map
            .get("deepseek.default")
            .expect("deepseek alias pricing");
        assert_eq!(deepseek.get("deepseek-v4-flash.input").copied(), Some(0.14));
        assert_eq!(
            deepseek.get("deepseek-v4-flash.cached_input").copied(),
            Some(0.0028)
        );
        assert_eq!(
            deepseek.get("deepseek-v4-flash.output").copied(),
            Some(0.28),
            "cost.rates must remain the canonical pricing source"
        );
    }

    #[test]
    fn build_model_provider_pricing_keeps_alias_legacy_pricing_isolated() {
        let mut config = Config::default();
        config.providers.models.deepseek.insert(
            "work".to_string(),
            DeepseekModelProviderConfig {
                base: ModelProviderConfig {
                    pricing: HashMap::from([("deepseek-v4-flash.output".into(), 0.77)]),
                    ..Default::default()
                },
            },
        );
        config.providers.models.deepseek.insert(
            "personal".to_string(),
            DeepseekModelProviderConfig {
                base: ModelProviderConfig {
                    pricing: HashMap::from([("deepseek-v4-flash.output".into(), 0.91)]),
                    ..Default::default()
                },
            },
        );

        let alias_map = build_model_provider_pricing(&config);
        let work = alias_map.get("deepseek.work").expect("work alias pricing");
        let personal = alias_map
            .get("deepseek.personal")
            .expect("personal alias pricing");

        assert_eq!(work.get("deepseek-v4-flash.output").copied(), Some(0.77));
        assert_eq!(
            personal.get("deepseek-v4-flash.output").copied(),
            Some(0.91)
        );
    }

    #[test]
    fn build_type_level_model_provider_pricing_merges_aliases_and_rate_sheet() {
        let mut config = Config::default();
        config.providers.models.deepseek.insert(
            "work".to_string(),
            DeepseekModelProviderConfig {
                base: ModelProviderConfig {
                    pricing: HashMap::from([
                        ("deepseek-v4-flash.input".into(), 0.33),
                        ("deepseek-v4-flash.output".into(), 0.77),
                    ]),
                    ..Default::default()
                },
            },
        );
        config.providers.models.deepseek.insert(
            "personal".to_string(),
            DeepseekModelProviderConfig {
                base: ModelProviderConfig {
                    pricing: HashMap::from([("deepseek-v4-flash.output".into(), 0.91)]),
                    ..Default::default()
                },
            },
        );
        config.cost.rates.providers.models.deepseek.insert(
            "deepseek-v4-flash".to_string(),
            zeroclaw_config::schema::ModelCostRates {
                input_per_mtok: Some(0.14),
                output_per_mtok: Some(0.28),
                cached_input_per_mtok: Some(0.0028),
            },
        );

        let by_type = build_type_level_model_provider_pricing(&config);
        let deepseek = by_type.get("deepseek").expect("deepseek type pricing");
        assert_eq!(deepseek.get("deepseek-v4-flash.input").copied(), Some(0.14));
        assert_eq!(
            deepseek.get("deepseek-v4-flash.output").copied(),
            Some(0.28)
        );
        assert_eq!(
            deepseek.get("deepseek-v4-flash.cached_input").copied(),
            Some(0.0028)
        );
    }

    #[test]
    fn build_type_level_model_provider_pricing_keeps_legacy_last_alias_wins_behavior() {
        let mut config = Config::default();
        config.providers.models.deepseek.insert(
            "work".to_string(),
            DeepseekModelProviderConfig {
                base: ModelProviderConfig {
                    pricing: HashMap::from([("deepseek-v4-flash.output".into(), 0.77)]),
                    ..Default::default()
                },
            },
        );
        config.providers.models.deepseek.insert(
            "personal".to_string(),
            DeepseekModelProviderConfig {
                base: ModelProviderConfig {
                    pricing: HashMap::from([("deepseek-v4-flash.output".into(), 0.91)]),
                    ..Default::default()
                },
            },
        );

        let mut expected = HashMap::new();
        for (type_k, _alias_k, profile) in config.providers.models.iter_entries() {
            if profile.pricing.is_empty() {
                continue;
            }
            let slot = expected
                .entry(type_k.to_string())
                .or_insert_with(HashMap::new);
            for (key, value) in &profile.pricing {
                slot.insert(key.clone(), *value);
            }
        }

        let by_type = build_type_level_model_provider_pricing(&config);
        let deepseek = by_type.get("deepseek").expect("deepseek type pricing");
        let expected_deepseek = expected.get("deepseek").expect("expected deepseek pricing");
        assert_eq!(deepseek, expected_deepseek);
    }

    #[test]
    fn record_tool_loop_cost_usage_applies_cached_input_pricing() {
        let workspace = tempfile::TempDir::new().unwrap();
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig::default(),
                workspace.path(),
            )
            .unwrap(),
        );
        let ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&tracker),
            Arc::new(HashMap::from([(
                "deepseek".to_string(),
                pricing_with_cache("deepseek-chat", 0.27, 0.027, 1.10),
            )])),
        );
        let usage = zeroclaw_providers::traits::TokenUsage {
            input_tokens: Some(5_000),
            output_tokens: Some(200),
            cached_input_tokens: Some(4_000),
        };

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (total_tokens, cost_usd) = runtime
            .block_on(TOOL_LOOP_COST_TRACKING_CONTEXT.scope(Some(ctx), async {
                record_tool_loop_cost_usage("deepseek", "deepseek-chat", &usage)
            }))
            .expect("cost usage");

        let expected = (1_000.0 * 0.27 / 1_000_000.0)
            + (4_000.0 * 0.027 / 1_000_000.0)
            + (200.0 * 1.10 / 1_000_000.0);
        assert_eq!(total_tokens, 5_200);
        assert!((cost_usd - expected).abs() < 1e-12);

        let stored = std::fs::read_to_string(workspace.path().join("state").join("costs.jsonl"))
            .expect("costs.jsonl should be written");
        let record: zeroclaw_config::cost::types::CostRecord =
            serde_json::from_str(stored.lines().next().expect("one record")).unwrap();
        assert_eq!(record.usage.cached_input_tokens, 4_000);
        assert_eq!(record.usage.billable_input_tokens(), 1_000);
    }

    #[test]
    fn record_tool_loop_cost_usage_persists_pricing_provenance() {
        let workspace = tempfile::TempDir::new().unwrap();
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig::default(),
                workspace.path(),
            )
            .unwrap(),
        );
        let ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&tracker),
            Arc::new(HashMap::from([(
                "configured-free".to_string(),
                pricing_with_cache("free-model", 0.0, 0.0, 0.0),
            )])),
        );
        let usage = zeroclaw_providers::traits::TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cached_input_tokens: Some(0),
        };

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(Some(ctx.clone()), async {
                    record_tool_loop_cost_usage("configured-free", "free-model", &usage)
                })
                .await
                .expect("configured-free usage");
            TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(Some(ctx), async {
                    record_tool_loop_cost_usage(
                        "missing-provider",
                        "missing-provenance-model",
                        &usage,
                    )
                })
                .await
                .expect("unpriced usage");
        });

        let stored = std::fs::read_to_string(workspace.path().join("state").join("costs.jsonl"))
            .expect("costs.jsonl should be written");
        let records: Vec<zeroclaw_config::cost::types::CostRecord> = stored
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert!(records[0].usage.pricing_available);
        assert_eq!(records[0].usage.cost_usd, 0.0);
        assert!(!records[1].usage.pricing_available);
        assert_eq!(records[1].usage.total_tokens, 150);
    }

    #[test]
    fn record_tool_loop_cost_usage_keeps_turn_usage_when_persistence_fails() {
        let workspace = tempfile::TempDir::new().unwrap();
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig::default(),
                workspace.path(),
            )
            .unwrap(),
        );
        std::fs::create_dir_all(workspace.path().join("state").join("costs.jsonl"))
            .expect("make costs.jsonl path unwritable as a directory");
        let ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&tracker),
            Arc::new(HashMap::from([(
                "deepseek".to_string(),
                pricing_with_cache("deepseek-chat", 0.27, 0.027, 1.10),
            )])),
        );
        let turn_usage = Arc::new(Mutex::new(TurnUsage::default()));
        let usage = zeroclaw_providers::traits::TokenUsage {
            input_tokens: Some(5_000),
            output_tokens: Some(200),
            cached_input_tokens: Some(4_000),
        };

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (total_tokens, cost_usd) = runtime
            .block_on(TOOL_LOOP_TURN_USAGE.scope(
                Some(Arc::clone(&turn_usage)),
                TOOL_LOOP_COST_TRACKING_CONTEXT.scope(Some(ctx), async {
                    record_tool_loop_cost_usage("deepseek", "deepseek-chat", &usage)
                }),
            ))
            .expect("cost usage");

        let expected = (1_000.0 * 0.27 / 1_000_000.0)
            + (4_000.0 * 0.027 / 1_000_000.0)
            + (200.0 * 1.10 / 1_000_000.0);
        assert_eq!(total_tokens, 5_200);
        assert!((cost_usd - expected).abs() < 1e-12);

        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 5_000);
        assert_eq!(recorded.output_tokens, 200);
        assert!((recorded.cost_usd - expected).abs() < 1e-12);
    }

    #[test]
    fn rejected_usage_accumulates_cost_without_replacing_context_window_fill() {
        let turn_usage = Arc::new(Mutex::new(TurnUsage::default()));
        let rejected = zeroclaw_providers::traits::TokenUsage {
            input_tokens: Some(80),
            output_tokens: Some(5),
            cached_input_tokens: Some(0),
        };
        let accepted = zeroclaw_providers::traits::TokenUsage {
            input_tokens: Some(80),
            output_tokens: Some(7),
            cached_input_tokens: Some(0),
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(TOOL_LOOP_TURN_USAGE.scope(
            Some(Arc::clone(&turn_usage)),
            TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                Some(ToolLoopCostTrackingContext::usage_only()),
                async {
                    assert!(
                        record_rejected_tool_loop_cost_usage("test", "test", &rejected).is_some()
                    );
                    assert!(record_tool_loop_cost_usage("test", "test", &accepted).is_some());
                },
            ),
        ));

        let usage = *turn_usage.lock();
        assert_eq!(usage.input_tokens, 160, "both attempts remain billed");
        assert_eq!(usage.output_tokens, 12, "both attempts remain billed");
        assert_eq!(
            usage.last_input_tokens, 80,
            "only the accepted attempt controls context-window fill"
        );
    }

    #[test]
    fn tool_loop_cost_tracking_context_from_tracker_stamps_given_alias() {
        // `tool_loop_cost_tracking_context_for_agent` (the builder
        // `send_message_to_peer.rs` calls for the peer-turn cost scope)
        // delegates to this function after resolving the process-global
        // tracker. Exercise it directly with an explicit LOCAL tracker so
        // this stays flake-free (no global-tracker singleton touched) while
        // still pinning the alias-stamping contract the per-agent
        // attribution depends on.
        let workspace = tempfile::TempDir::new().unwrap();
        let tracker = Arc::new(
            CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: true,
                    track_per_agent: true,
                    ..zeroclaw_config::schema::CostConfig::default()
                },
                workspace.path(),
            )
            .unwrap(),
        );

        let ctx = tool_loop_cost_tracking_context_from_tracker(
            &Config::default(),
            "peer-recipient",
            tracker,
        );

        assert_eq!(ctx.agent_alias, Some("peer-recipient".to_string()));
    }
}
