//! Pricing module for computing token costs across providers and models.
//!
//! This module provides a data-driven pricing lookup system with fallbacks:
//! 1. Exact match: (provider, model)
//! 2. Provider fallback: (provider, "*")
//! 3. Global fallback: ("*", "*")
//!
//! Adding a new model/provider only requires adding a pricing entry.

use std::collections::HashMap;

/// Pricing per 1M tokens for a model.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelPricing {
    /// USD per 1M input tokens.
    pub input_per_m: f64,
    /// USD per 1M output tokens.
    pub output_per_m: f64,
    /// USD per 1M cache read tokens (optional, often discounted).
    pub cache_read_per_m: Option<f64>,
    /// USD per 1M cache write tokens (optional, often discounted).
    pub cache_write_per_m: Option<f64>,
}

impl ModelPricing {
    /// Create pricing with input and output rates.
    pub fn new(input_per_m: f64, output_per_m: f64) -> Self {
        Self {
            input_per_m,
            output_per_m,
            cache_read_per_m: None,
            cache_write_per_m: None,
        }
    }

    /// Add cache read pricing.
    pub fn with_cache_read(mut self, per_m: f64) -> Self {
        self.cache_read_per_m = Some(per_m);
        self
    }

    /// Add cache write pricing.
    pub fn with_cache_write(mut self, per_m: f64) -> Self {
        self.cache_write_per_m = Some(per_m);
        self
    }

    /// Compute cost for given token counts.
    pub fn compute_cost(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> f64 {
        let input_cost = (input_tokens as f64 / 1_000_000.0) * self.input_per_m;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * self.output_per_m;

        let cache_read_cost = self.cache_read_per_m
            .map(|rate| (cache_read_tokens as f64 / 1_000_000.0) * rate)
            .unwrap_or(0.0);

        let cache_write_cost = self.cache_write_per_m
            .map(|rate| (cache_write_tokens as f64 / 1_000_000.0) * rate)
            .unwrap_or(0.0);

        input_cost + output_cost + cache_read_cost + cache_write_cost
    }
}

/// Global pricing registry.
pub struct PricingRegistry {
    /// Maps (provider, model) -> pricing.
    /// Key format: "provider:model" (lowercase).
    pricing: HashMap<String, ModelPricing>,
}

impl Default for PricingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PricingRegistry {
    /// Create a new pricing registry with default pricing data.
    pub fn new() -> Self {
        let mut registry = Self {
            pricing: HashMap::new(),
        };
        registry.populate_defaults();
        registry
    }

    /// Add pricing for a specific provider/model combination.
    pub fn add(&mut self, provider: &str, model: &str, pricing: ModelPricing) {
        let key = format!("{}:{}", provider.to_lowercase(), model.to_lowercase());
        self.pricing.insert(key, pricing);
    }

    /// Look up pricing with fallbacks:
    /// 1. Exact: "provider:model"
    /// 2. Provider fallback: "provider:*"
    /// 3. Global fallback: "*:*"
    pub fn lookup(&self, provider: &str, model: &str) -> Option<ModelPricing> {
        let provider_lower = provider.to_lowercase();
        let model_lower = model.to_lowercase();

        // 1. Exact match
        let exact_key = format!("{}:{}", provider_lower, model_lower);
        if let Some(pricing) = self.pricing.get(&exact_key) {
            return Some(*pricing);
        }

        // 2. Provider fallback
        let provider_key = format!("{}:*", provider_lower);
        if let Some(pricing) = self.pricing.get(&provider_key) {
            return Some(*pricing);
        }

        // 3. Global fallback
        self.pricing.get("*:*").copied()
    }

    /// Compute cost for a given provider/model/token combination.
    /// Returns None if no pricing found.
    pub fn compute_cost(
        &self,
        provider: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> Option<f64> {
        let pricing = self.lookup(provider, model)?;
        Some(pricing.compute_cost(input_tokens, output_tokens, cache_read_tokens, cache_write_tokens))
    }

    /// Populate default pricing for known providers/models.
    fn populate_defaults(&mut self) {
        // ===== MOONSHOT / KIMI =====
        // moonshot-intl:kimi-k2.5 via OpenRouter
        // Pricing based on Moonshot API pricing (confirm with current rates)
        self.add(
            "openrouter",
            "moonshot-intl/kimi-k2.5",
            ModelPricing::new(0.6, 2.5), // $0.6/1M input, $2.5/1M output
        );
        // Alias without openrouter prefix
        self.add(
            "moonshot-intl",
            "kimi-k2.5",
            ModelPricing::new(0.6, 2.5),
        );
        // Catch-all for kimi models
        self.add(
            "openrouter",
            "moonshot-intl/*",
            ModelPricing::new(0.6, 2.5),
        );

        // ===== ANTHROPIC CLAUDE =====
        self.add(
            "anthropic",
            "claude-sonnet-4",
            ModelPricing::new(3.0, 15.0),
        );
        self.add(
            "anthropic",
            "claude-3-5-sonnet",
            ModelPricing::new(3.0, 15.0),
        );
        self.add(
            "anthropic",
            "claude-3-5-sonnet-20241022",
            ModelPricing::new(3.0, 15.0),
        );
        self.add(
            "anthropic",
            "claude-3-5-haiku",
            ModelPricing::new(0.8, 4.0),
        );
        self.add(
            "anthropic",
            "claude-3-opus",
            ModelPricing::new(15.0, 75.0),
        );
        self.add(
            "anthropic",
            "claude-opus-4",
            ModelPricing::new(15.0, 75.0),
        );
        // Claude with prompt caching
        self.add(
            "anthropic",
            "claude-3-5-sonnet-20241022-cached",
            ModelPricing::new(3.0, 15.0)
                .with_cache_read(0.3)  // 90% discount on cache reads
                .with_cache_write(3.75), // 25% premium on cache writes
        );

        // ===== OPENAI =====
        self.add(
            "openai",
            "gpt-4o",
            ModelPricing::new(2.5, 10.0),
        );
        self.add(
            "openai",
            "gpt-4o-mini",
            ModelPricing::new(0.15, 0.6),
        );
        self.add(
            "openai",
            "gpt-4-turbo",
            ModelPricing::new(10.0, 30.0),
        );
        self.add(
            "openai",
            "o1",
            ModelPricing::new(15.0, 60.0),
        );
        self.add(
            "openai",
            "o1-mini",
            ModelPricing::new(1.5, 6.0),
        );
        self.add(
            "openai",
            "o3-mini",
            ModelPricing::new(1.1, 4.4),
        );

        // ===== OPENROUTER FALLBACKS =====
        // OpenRouter generally passes through provider pricing, but we set sane defaults
        self.add(
            "openrouter",
            "anthropic/claude-3.5-sonnet",
            ModelPricing::new(3.0, 15.0),
        );
        self.add(
            "openrouter",
            "anthropic/claude-3-opus",
            ModelPricing::new(15.0, 75.0),
        );
        self.add(
            "openrouter",
            "openai/gpt-4o",
            ModelPricing::new(2.5, 10.0),
        );
        self.add(
            "openrouter",
            "openai/gpt-4o-mini",
            ModelPricing::new(0.15, 0.6),
        );

        // ===== PROVIDER FALLBACKS =====
        // Used when model name doesn't match exactly
        self.add("anthropic", "*", ModelPricing::new(3.0, 15.0));
        self.add("openai", "*", ModelPricing::new(2.5, 10.0));
        self.add("openrouter", "*", ModelPricing::new(1.0, 3.0));

        // ===== GLOBAL FALLBACK =====
        // Very conservative default for unknown providers
        self.add("*", "*", ModelPricing::new(1.0, 3.0));
    }
}

/// Global singleton pricing registry.
static PRICING: std::sync::OnceLock<PricingRegistry> = std::sync::OnceLock::new();

/// Get the global pricing registry.
pub fn pricing() -> &'static PricingRegistry {
    PRICING.get_or_init(PricingRegistry::new)
}

/// Convenience function to compute cost.
pub fn compute_cost(
    provider: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> f64 {
    pricing()
        .compute_cost(provider, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)
        .unwrap_or(0.0)
}

/// Convenience function to compute usage with cost.
/// Returns a Usage struct with provider, model, tokens, and computed cost.
pub fn compute_usage_with_cost(
    provider: impl Into<String>,
    model: impl Into<String>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> crate::providers::traits::Usage {
    let provider_str = provider.into();
    let model_str = model.into();
    let cost = compute_cost(&provider_str, &model_str, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens);

    crate::providers::traits::Usage {
        provider: provider_str,
        model: model_str,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cost_usd: cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let cost = compute_cost("anthropic", "claude-sonnet-4", 1_000_000, 500_000, 0, 0);
        // $3/1M input + $15/1M output * 0.5 = $3 + $7.5 = $10.50
        assert!((cost - 10.5).abs() < 0.01);
    }

    #[test]
    fn test_provider_fallback() {
        let cost = compute_cost("anthropic", "unknown-model", 1_000_000, 0, 0, 0);
        // Falls back to anthropic:* -> $3/1M
        assert!((cost - 3.0).abs() < 0.01);
    }

    #[test]
    fn test_global_fallback() {
        let cost = compute_cost("unknown-provider", "unknown-model", 1_000_000, 0, 0, 0);
        // Falls back to *:* -> $1/1M
        assert!((cost - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_moonshot_kimi() {
        let cost = compute_cost("openrouter", "moonshot-intl/kimi-k2.5", 1_000_000, 1_000_000, 0, 0);
        // $0.6/1M input + $2.5/1M output = $3.10
        assert!((cost - 3.1).abs() < 0.01);
    }

    #[test]
    fn test_cache_pricing() {
        let mut registry = PricingRegistry::new();
        registry.add(
            "test",
            "cached-model",
            ModelPricing::new(3.0, 15.0)
                .with_cache_read(0.3)
                .with_cache_write(3.75),
        );

        let cost = registry
            .compute_cost("test", "cached-model", 1_000_000, 500_000, 2_000_000, 1_000_000)
            .unwrap();

        // Input: $3
        // Output: $7.5
        // Cache read: $0.6 (2M * $0.3/1M)
        // Cache write: $3.75 (1M * $3.75/1M)
        // Total: $14.85
        assert!((cost - 14.85).abs() < 0.01);
    }

    #[test]
    fn test_zero_tokens() {
        let cost = compute_cost("anthropic", "claude-sonnet-4", 0, 0, 0, 0);
        assert!((cost - 0.0).abs() < 0.001);
    }
}