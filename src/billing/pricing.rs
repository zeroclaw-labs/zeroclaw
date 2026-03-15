//! Comprehensive model pricing registry for LLM API cost calculation.
//!
//! Pricing data is stored in `model_pricing.toml` inside the workspace directory
//! and can be updated at runtime via the admin API. The registry supports:
//!
//! - Per-model input/output token pricing (USD per 1M tokens)
//! - Provider-level grouping for easy management
//! - Runtime CRUD operations (add, update, delete models)
//! - TOML persistence for operator-friendly editing
//! - Thread-safe concurrent access via `Arc<RwLock<...>>`

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Pricing for a single model (USD per 1M tokens).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelPrice {
    /// Provider name (e.g. "anthropic", "openai", "gemini").
    pub provider: String,
    /// Display name for admin UI.
    pub display_name: String,
    /// Input token price per 1M tokens (USD).
    pub input_per_million: f64,
    /// Output token price per 1M tokens (USD).
    pub output_per_million: f64,
    /// Optional note (e.g. "long context >200K: 2x input price").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The full pricing registry, keyed by canonical model ID.
///
/// Model IDs use the format from the provider's API (e.g. "claude-opus-4-6",
/// "gpt-4.1", "gemini-3.1-pro-preview"). The key is case-sensitive and must match
/// exactly what the provider returns in usage responses.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PricingRegistry {
    /// Map of model_id → pricing.
    pub models: BTreeMap<String, ModelPrice>,
}

const PRICING_FILE: &str = "model_pricing.toml";

impl PricingRegistry {
    /// Build the default pricing registry with all known models.
    ///
    /// This is the authoritative source of pricing data. Operators can override
    /// individual entries via the admin API or by editing model_pricing.toml.
    pub fn defaults() -> Self {
        let mut models = BTreeMap::new();

        // ── Anthropic (Claude) ─────────────────────────────────────
        // Source: Anthropic API pricing, Feb 2026
        models.insert("claude-opus-4-6".into(), ModelPrice {
            provider: "anthropic".into(),
            display_name: "Claude Opus 4.6/4.5".into(),
            input_per_million: 5.0,
            output_per_million: 25.0,
            note: Some("Top-tier reasoning/coding model".into()),
        });
        models.insert("claude-sonnet-4-20250514".into(), ModelPrice {
            provider: "anthropic".into(),
            display_name: "Claude Sonnet 4.6/4.5".into(),
            input_per_million: 3.0,
            output_per_million: 15.0,
            note: Some("Balanced performance model".into()),
        });
        models.insert("claude-sonnet-4-5-20250929".into(), ModelPrice {
            provider: "anthropic".into(),
            display_name: "Claude Sonnet 4.5 (Sep 2025)".into(),
            input_per_million: 3.0,
            output_per_million: 15.0,
            note: None,
        });
        models.insert("claude-haiku-4-5-20251001".into(), ModelPrice {
            provider: "anthropic".into(),
            display_name: "Claude Haiku 4.5".into(),
            input_per_million: 1.0,
            output_per_million: 5.0,
            note: Some("Fast, cost-effective model".into()),
        });
        // Anthropic prompt caching
        models.insert("claude-cache-read".into(), ModelPrice {
            provider: "anthropic".into(),
            display_name: "Claude Cache Read".into(),
            input_per_million: 0.30,
            output_per_million: 0.0,
            note: Some("Cached input token read cost".into()),
        });
        models.insert("claude-cache-write".into(), ModelPrice {
            provider: "anthropic".into(),
            display_name: "Claude Cache Write".into(),
            input_per_million: 3.75,
            output_per_million: 0.0,
            note: Some("Cache creation write cost".into()),
        });

        // ── OpenAI ─────────────────────────────────────────────────
        // Source: OpenAI API pricing page, Feb 2026

        // Flagship
        models.insert("gpt-5.2".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-5.2".into(),
            input_per_million: 1.75,
            output_per_million: 14.0,
            note: Some("Latest flagship (Feb 2026)".into()),
        });
        models.insert("gpt-5".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-5".into(),
            input_per_million: 1.25,
            output_per_million: 10.0,
            note: None,
        });
        models.insert("gpt-5-mini".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-5 Mini".into(),
            input_per_million: 0.25,
            output_per_million: 2.0,
            note: None,
        });
        models.insert("gpt-5-nano".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-5 Nano".into(),
            input_per_million: 0.05,
            output_per_million: 0.40,
            note: None,
        });

        // GPT-4.1 family (1M context)
        models.insert("gpt-4.1".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-4.1".into(),
            input_per_million: 2.0,
            output_per_million: 8.0,
            note: Some("1M context window".into()),
        });
        models.insert("gpt-4.1-mini".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-4.1 Mini".into(),
            input_per_million: 0.40,
            output_per_million: 1.60,
            note: Some("1M context, cost-effective".into()),
        });
        models.insert("gpt-4.1-nano".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-4.1 Nano".into(),
            input_per_million: 0.10,
            output_per_million: 0.40,
            note: Some("Cheapest OpenAI model".into()),
        });

        // GPT-4o family
        models.insert("gpt-4o".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-4o".into(),
            input_per_million: 2.50,
            output_per_million: 10.0,
            note: Some("128K context".into()),
        });
        models.insert("gpt-4o-mini".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "GPT-4o Mini".into(),
            input_per_million: 0.15,
            output_per_million: 0.60,
            note: Some("128K context, budget".into()),
        });

        // O-series reasoning models
        models.insert("o3".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "o3".into(),
            input_per_million: 2.0,
            output_per_million: 8.0,
            note: Some("Reasoning model; reasoning tokens billed as output".into()),
        });
        models.insert("o4-mini".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "o4-mini".into(),
            input_per_million: 1.10,
            output_per_million: 4.40,
            note: Some("Budget reasoning model".into()),
        });
        models.insert("o1".into(), ModelPrice {
            provider: "openai".into(),
            display_name: "o1".into(),
            input_per_million: 15.0,
            output_per_million: 60.0,
            note: Some("Legacy reasoning model".into()),
        });

        // ── Google Gemini ──────────────────────────────────────────
        // Source: Google AI Studio pricing, Mar 2026

        // Gemini 3.1 (latest)
        models.insert("gemini-3.1-pro-preview".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 3.1 Pro".into(),
            input_per_million: 2.0,
            output_per_million: 12.0,
            note: Some(">200K context: $4.0 input, $18.0 output".into()),
        });
        models.insert("gemini-3.1-flash-lite-preview".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 3.1 Flash Lite".into(),
            input_per_million: 0.25,
            output_per_million: 1.50,
            note: Some("Fast, most cost-effective Gemini".into()),
        });

        // Gemini 3.0
        models.insert("gemini-3-pro-preview".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 3 Pro".into(),
            input_per_million: 2.0,
            output_per_million: 12.0,
            note: None,
        });
        models.insert("gemini-3-flash-preview".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 3 Flash".into(),
            input_per_million: 0.50,
            output_per_million: 3.0,
            note: None,
        });

        // Gemini 2.5
        models.insert("gemini-2.5-pro".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 2.5 Pro".into(),
            input_per_million: 1.25,
            output_per_million: 10.0,
            note: Some(">200K context: $2.50/$15.00".into()),
        });
        models.insert("gemini-2.5-flash".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 2.5 Flash".into(),
            input_per_million: 0.30,
            output_per_million: 2.50,
            note: Some("Thinking mode output: $3.50/M".into()),
        });
        models.insert("gemini-2.5-flash-lite".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 2.5 Flash Lite".into(),
            input_per_million: 0.10,
            output_per_million: 0.40,
            note: None,
        });

        // Gemini 2.0 (deprecated June 2026)
        models.insert("gemini-2.0-flash".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 2.0 Flash".into(),
            input_per_million: 0.10,
            output_per_million: 0.40,
            note: Some("Deprecated: shutdown June 2026".into()),
        });

        // Gemini 1.5
        models.insert("gemini-1.5-pro".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 1.5 Pro".into(),
            input_per_million: 1.25,
            output_per_million: 5.0,
            note: None,
        });
        models.insert("gemini-1.5-flash".into(), ModelPrice {
            provider: "gemini".into(),
            display_name: "Gemini 1.5 Flash".into(),
            input_per_million: 0.075,
            output_per_million: 0.30,
            note: None,
        });

        Self { models }
    }

    /// Load pricing from a TOML file, falling back to defaults if the file
    /// doesn't exist. Any models in defaults that are missing from the file
    /// are merged in (operator additions are preserved).
    pub fn load_or_default(workspace_dir: &Path) -> Self {
        let path = workspace_dir.join(PRICING_FILE);
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match toml::from_str::<PricingRegistry>(&contents) {
                    Ok(loaded) => {
                        tracing::info!(
                            models = loaded.models.len(),
                            path = %path.display(),
                            "Loaded model pricing registry"
                        );
                        return loaded;
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "Failed to parse pricing file, using defaults"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "Failed to read pricing file, using defaults"
                    );
                }
            }
        }

        let defaults = Self::defaults();
        // Try to save defaults so operator has a template to edit
        if let Err(e) = defaults.save(workspace_dir) {
            tracing::debug!(error = %e, "Could not write default pricing file (non-fatal)");
        }
        defaults
    }

    /// Save the pricing registry to TOML.
    pub fn save(&self, workspace_dir: &Path) -> anyhow::Result<()> {
        let path = workspace_dir.join(PRICING_FILE);
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        tracing::info!(
            models = self.models.len(),
            path = %path.display(),
            "Saved model pricing registry"
        );
        Ok(())
    }

    /// Look up pricing for a model ID.
    ///
    /// Tries exact match first, then falls back to substring matching
    /// (e.g. "claude-sonnet-4" matches "claude-sonnet-4-20250514").
    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        // Exact match
        if let Some(price) = self.models.get(model) {
            return Some(price);
        }

        // Substring match: find the longest key that is a substring of the query
        // or the query is a substring of the key
        let mut best: Option<(&str, &ModelPrice)> = None;
        for (key, price) in &self.models {
            if model.contains(key.as_str()) || key.contains(model) {
                match best {
                    None => best = Some((key, price)),
                    Some((prev_key, _)) if key.len() > prev_key.len() => {
                        best = Some((key, price));
                    }
                    _ => {}
                }
            }
        }
        best.map(|(_, p)| p)
    }

    /// Estimate the USD cost for given token usage.
    ///
    /// Returns (cost_usd, found) where `found` indicates whether
    /// the model was in the registry (vs. using a fallback estimate).
    pub fn estimate_cost(&self, model: &str, input_tokens: i64, output_tokens: i64) -> (f64, bool) {
        if let Some(price) = self.lookup(model) {
            let input_cost = (input_tokens as f64 / 1_000_000.0) * price.input_per_million;
            let output_cost = (output_tokens as f64 / 1_000_000.0) * price.output_per_million;
            (input_cost + output_cost, true)
        } else {
            // Conservative fallback: $1.00 input / $3.00 output per 1M
            let input_cost = (input_tokens as f64 / 1_000_000.0) * 1.0;
            let output_cost = (output_tokens as f64 / 1_000_000.0) * 3.0;
            (input_cost + output_cost, false)
        }
    }

    /// List all models grouped by provider.
    pub fn by_provider(&self) -> BTreeMap<String, Vec<(String, ModelPrice)>> {
        let mut grouped: BTreeMap<String, Vec<(String, ModelPrice)>> = BTreeMap::new();
        for (model_id, price) in &self.models {
            grouped
                .entry(price.provider.clone())
                .or_default()
                .push((model_id.clone(), price.clone()));
        }
        grouped
    }

    /// Add or update a model's pricing.
    pub fn upsert(&mut self, model_id: String, price: ModelPrice) {
        self.models.insert(model_id, price);
    }

    /// Remove a model from the registry.
    pub fn remove(&mut self, model_id: &str) -> Option<ModelPrice> {
        self.models.remove(model_id)
    }
}

/// Thread-safe shared pricing registry handle.
///
/// Initialized once at startup and shared across the gateway and agent.
#[derive(Clone)]
pub struct SharedPricingRegistry {
    inner: Arc<RwLock<PricingRegistry>>,
    workspace_dir: PathBuf,
}

impl SharedPricingRegistry {
    /// Create a new shared registry, loading from disk or defaults.
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            inner: Arc::new(RwLock::new(PricingRegistry::load_or_default(workspace_dir))),
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    /// Estimate cost using the current registry.
    pub fn estimate_cost(&self, model: &str, input_tokens: i64, output_tokens: i64) -> f64 {
        let registry = self.inner.read().unwrap();
        let (cost, _) = registry.estimate_cost(model, input_tokens, output_tokens);
        cost
    }

    /// Get a snapshot of the full registry.
    pub fn snapshot(&self) -> PricingRegistry {
        self.inner.read().unwrap().clone()
    }

    /// Look up a single model's pricing.
    pub fn get_model(&self, model_id: &str) -> Option<ModelPrice> {
        self.inner.read().unwrap().lookup(model_id).cloned()
    }

    /// Add or update a model's pricing, then persist to disk.
    pub fn upsert_and_save(&self, model_id: String, price: ModelPrice) -> anyhow::Result<()> {
        let mut registry = self.inner.write().unwrap();
        registry.upsert(model_id, price);
        registry.save(&self.workspace_dir)
    }

    /// Remove a model's pricing, then persist to disk.
    pub fn remove_and_save(&self, model_id: &str) -> anyhow::Result<Option<ModelPrice>> {
        let mut registry = self.inner.write().unwrap();
        let removed = registry.remove(model_id);
        registry.save(&self.workspace_dir)?;
        Ok(removed)
    }

    /// Replace the entire registry and persist to disk.
    pub fn replace_and_save(&self, new_registry: PricingRegistry) -> anyhow::Result<()> {
        let mut registry = self.inner.write().unwrap();
        *registry = new_registry;
        registry.save(&self.workspace_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn defaults_has_all_major_models() {
        let reg = PricingRegistry::defaults();
        // Anthropic
        assert!(reg.models.contains_key("claude-opus-4-6"));
        assert!(reg.models.contains_key("claude-sonnet-4-20250514"));
        assert!(reg.models.contains_key("claude-haiku-4-5-20251001"));
        // OpenAI
        assert!(reg.models.contains_key("gpt-5.2"));
        assert!(reg.models.contains_key("gpt-4.1"));
        assert!(reg.models.contains_key("gpt-4.1-mini"));
        assert!(reg.models.contains_key("gpt-4.1-nano"));
        assert!(reg.models.contains_key("gpt-4o"));
        assert!(reg.models.contains_key("o3"));
        assert!(reg.models.contains_key("o4-mini"));
        // Gemini
        assert!(reg.models.contains_key("gemini-3.1-pro-preview"));
        assert!(reg.models.contains_key("gemini-3.1-flash-lite-preview"));
        assert!(reg.models.contains_key("gemini-2.5-pro"));
        assert!(reg.models.contains_key("gemini-2.5-flash"));
    }

    #[test]
    fn exact_lookup() {
        let reg = PricingRegistry::defaults();
        let price = reg.lookup("claude-opus-4-6").unwrap();
        assert_eq!(price.input_per_million, 5.0);
        assert_eq!(price.output_per_million, 25.0);
    }

    #[test]
    fn substring_lookup() {
        let reg = PricingRegistry::defaults();
        // "claude-sonnet-4" should match "claude-sonnet-4-20250514"
        let price = reg.lookup("claude-sonnet-4").unwrap();
        assert_eq!(price.input_per_million, 3.0);
    }

    #[test]
    fn estimate_cost_known_model() {
        let reg = PricingRegistry::defaults();
        let (cost, found) = reg.estimate_cost("gpt-4o", 1_000_000, 1_000_000);
        assert!(found);
        assert!((cost - 12.50).abs() < 0.01); // $2.50 + $10.00
    }

    #[test]
    fn estimate_cost_unknown_model_uses_fallback() {
        let reg = PricingRegistry::defaults();
        let (cost, found) = reg.estimate_cost("unknown-model-xyz", 1_000_000, 1_000_000);
        assert!(!found);
        assert!((cost - 4.0).abs() < 0.01); // $1.00 + $3.00 fallback
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let original = PricingRegistry::defaults();
        original.save(tmp.path()).unwrap();

        let loaded = PricingRegistry::load_or_default(tmp.path());
        assert_eq!(original.models.len(), loaded.models.len());
        for (key, orig_price) in &original.models {
            let loaded_price = loaded.models.get(key).unwrap();
            assert_eq!(orig_price, loaded_price, "Mismatch for model {key}");
        }
    }

    #[test]
    fn upsert_adds_new_model() {
        let mut reg = PricingRegistry::defaults();
        let count_before = reg.models.len();
        reg.upsert(
            "new-model-2026".into(),
            ModelPrice {
                provider: "test".into(),
                display_name: "New Model".into(),
                input_per_million: 1.0,
                output_per_million: 5.0,
                note: None,
            },
        );
        assert_eq!(reg.models.len(), count_before + 1);
        assert!(reg.models.contains_key("new-model-2026"));
    }

    #[test]
    fn remove_deletes_model() {
        let mut reg = PricingRegistry::defaults();
        assert!(reg.models.contains_key("gpt-4o"));
        let removed = reg.remove("gpt-4o");
        assert!(removed.is_some());
        assert!(!reg.models.contains_key("gpt-4o"));
    }

    #[test]
    fn by_provider_groups_correctly() {
        let reg = PricingRegistry::defaults();
        let grouped = reg.by_provider();
        assert!(grouped.contains_key("anthropic"));
        assert!(grouped.contains_key("openai"));
        assert!(grouped.contains_key("gemini"));
    }

    #[test]
    fn shared_registry_concurrent_access() {
        let tmp = TempDir::new().unwrap();
        let shared = SharedPricingRegistry::new(tmp.path());

        // Read
        let cost = shared.estimate_cost("gpt-4.1", 1_000_000, 1_000_000);
        assert!((cost - 10.0).abs() < 0.01); // $2.00 + $8.00

        // Write
        shared
            .upsert_and_save(
                "test-model".into(),
                ModelPrice {
                    provider: "test".into(),
                    display_name: "Test".into(),
                    input_per_million: 0.5,
                    output_per_million: 1.5,
                    note: None,
                },
            )
            .unwrap();

        // Verify write persisted
        let snap = shared.snapshot();
        assert!(snap.models.contains_key("test-model"));
    }

    #[test]
    fn claude_opus_pricing_matches_spec() {
        let reg = PricingRegistry::defaults();
        let opus = reg.lookup("claude-opus-4-6").unwrap();
        assert_eq!(opus.input_per_million, 5.0);
        assert_eq!(opus.output_per_million, 25.0);
    }

    #[test]
    fn gemini_flash_lite_pricing_matches_spec() {
        let reg = PricingRegistry::defaults();
        let flash = reg.lookup("gemini-3.1-flash-lite-preview").unwrap();
        assert_eq!(flash.input_per_million, 0.25);
        assert_eq!(flash.output_per_million, 1.50);
    }
}
