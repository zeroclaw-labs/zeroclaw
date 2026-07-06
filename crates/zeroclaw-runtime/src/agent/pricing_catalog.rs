//! Global pricing catalog: a last-resort fallback consulted by the cost engine
//! when a model has no per-provider `pricing` entry in config.
//!
//! Loaded from `<data_dir>/pricing.json` if present. Populating that file is a
//! build-specific CLI concern, never a public-feed fetch inside this shared
//! daemon. A typical setup writes it from a public price feed (e.g. LiteLLM /
//! OpenRouter) on a schedule; an air-gapped or self-hosted setup may write only
//! the rates it cares about, or no file at all.
//!
//! When the file is present this makes `cost_usd` non-zero for cloud models the
//! operator never hand-priced in `config.toml`. When absent the engine prices
//! nothing here and self-hosted/free models stay `$0`.
//!
//! Matching is intentionally **exact and case-insensitive on the full model id
//! only** — no leaf/substring fuzzy. Provider-qualified ids for self-hosted or
//! private deployments (e.g. `myhost/...`) are typically absent from a public
//! catalog, so an exact-only match leaves them at `$0` (correct: they are not
//! billed). A broad fuzzy match would misprice a free self-hosted
//! `gpt-oss-120b` against its public rate, or collapse `grok-4.3` onto
//! `grok-4`; exact matching avoids both.

use parking_lot::RwLock;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// One model's chargeback rates, mirroring the on-disk `pricing.json` shape
/// written by the pricing feed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogModelPrice {
    /// Blended USD per 1M tokens (input+output), used only when the per-
    /// component rates below are absent.
    #[serde(default)]
    pub usd_per_mtok: f64,
    #[serde(default)]
    pub input_usd_per_mtok: f64,
    #[serde(default)]
    pub output_usd_per_mtok: f64,
    #[serde(default)]
    pub cache_read_usd_per_mtok: f64,
    #[serde(default)]
    pub cache_write_usd_per_mtok: f64,
    #[serde(default)]
    pub reasoning_usd_per_mtok: f64,
    #[serde(default)]
    pub source: String,
}

impl CatalogModelPrice {
    /// `(input, output, cached_input)` per-1M-token rates. Falls back to the
    /// blended `usd_per_mtok` for both input and output when no per-component
    /// rate is present.
    fn rates(&self) -> (f64, f64, f64) {
        let (mut input, mut output) = (self.input_usd_per_mtok, self.output_usd_per_mtok);
        if input == 0.0 && output == 0.0 && self.usd_per_mtok > 0.0 {
            input = self.usd_per_mtok;
            output = self.usd_per_mtok;
        }
        (input, output, self.cache_read_usd_per_mtok)
    }

    fn is_priced(&self) -> bool {
        self.usd_per_mtok > 0.0 || self.input_usd_per_mtok > 0.0 || self.output_usd_per_mtok > 0.0
    }
}

/// The full catalog as loaded from `pricing.json`, plus a precomputed
/// lowercase index for O(1) exact lookups on the hot path.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GlobalPricingCatalog {
    #[serde(default)]
    pub generated: String,
    #[serde(default)]
    pub window: String,
    #[serde(default)]
    pub models: HashMap<String, CatalogModelPrice>,
    #[serde(default)]
    pub baseline_usd_per_mtok: f64,
    #[serde(default)]
    pub baseline_model: String,
    /// Lowercased full-id -> rates index, built by [`Self::finalize`]. Only
    /// priced models are included so an exact hit always yields a non-zero
    /// rate.
    #[serde(skip)]
    index: HashMap<String, (f64, f64, f64)>,
}

impl GlobalPricingCatalog {
    /// Build the lowercase exact-match index. Call once after deserialize.
    fn finalize(&mut self) {
        let mut index = HashMap::with_capacity(self.models.len());
        for (id, price) in &self.models {
            if price.is_priced() {
                index.insert(id.to_ascii_lowercase(), price.rates());
            }
        }
        self.index = index;
    }

    /// Number of priced models in the catalog.
    pub fn priced_len(&self) -> usize {
        self.index.len()
    }

    /// Exact, case-insensitive full-id lookup. Returns `(input, output,
    /// cached_input)` per-1M-token rates, or `None` when the model is not in
    /// the catalog (which keeps provider-qualified self-hosted ids at `$0`).
    pub fn rates_for(&self, model: &str) -> Option<(f64, f64, f64)> {
        self.index.get(&model.to_ascii_lowercase()).copied()
    }
}

fn cell() -> &'static RwLock<Arc<GlobalPricingCatalog>> {
    static GLOBAL: OnceLock<RwLock<Arc<GlobalPricingCatalog>>> = OnceLock::new();
    GLOBAL.get_or_init(|| RwLock::new(Arc::new(GlobalPricingCatalog::default())))
}

/// Replace the process-global catalog (used by startup load and config reload
/// to hot-swap rates without a restart). The catalog file itself is refreshed
/// out-of-band by the CLI + launchd, not by this daemon.
pub fn set_global_pricing_catalog(mut catalog: GlobalPricingCatalog) {
    catalog.finalize();
    *cell().write() = Arc::new(catalog);
}

/// Current process-global catalog snapshot.
pub fn global_pricing_catalog() -> Arc<GlobalPricingCatalog> {
    cell().read().clone()
}

/// Exact per-1M-token rates for `model` from the global catalog, or `None`.
pub fn global_pricing_rates(model: &str) -> Option<(f64, f64, f64)> {
    global_pricing_catalog().rates_for(model)
}

/// Load `<data_dir>/pricing.json` into the process-global catalog. Returns the
/// number of priced models loaded. A MISSING file clears the global catalog
/// (so removing `pricing.json` reverts unmatched models to `$0` on the next
/// reload, even same-PID); a corrupt or otherwise unreadable file keeps the
/// previous catalog (never fatal — an unpriced run simply reports `$0`).
pub fn load_global_pricing_catalog(data_dir: &Path) -> usize {
    let path = data_dir.join("pricing.json");
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<GlobalPricingCatalog>(&contents) {
            Ok(catalog) => {
                let mut catalog = catalog;
                catalog.finalize();
                let n = catalog.priced_len();
                *cell().write() = Arc::new(catalog);
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "path": path.display().to_string(),
                            "priced_models": n
                        })),
                    "Loaded global pricing catalog"
                );
                n
            }
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "path": path.display().to_string(),
                            "error": format!("{error}")
                        })),
                    "Pricing catalog unreadable; keeping previous rates"
                );
                global_pricing_catalog().priced_len()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // No catalog file: clear any previously-loaded global catalog so removing
            // <data_dir>/pricing.json reverts unmatched models to $0 on the next reload
            // (same PID), honoring the documented rollback contract.
            set_global_pricing_catalog(GlobalPricingCatalog::default());
            0
        }
        Err(error) => {
            // Present but unreadable (permissions, a directory, transient I/O): keep the
            // previous catalog rather than silently dropping configured rates.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "path": path.display().to_string(),
                        "error": format!("{error}")
                    })),
                "Pricing catalog unreadable; keeping previous rates"
            );
            global_pricing_catalog().priced_len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_clears_global_catalog_on_same_process_reload() {
        // Rollback contract: deleting <data_dir>/pricing.json must revert unmatched
        // models to $0 on the next reload, even in the same PID (the global catalog is
        // process-global across config reloads).
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("pricing.json");
        std::fs::write(
            &path,
            r#"{"models":{"acme/rollback-probe":{"input_usd_per_mtok":1.0,"output_usd_per_mtok":2.0}}}"#,
        )
        .unwrap();
        load_global_pricing_catalog(tmp.path());
        assert!(
            global_pricing_rates("acme/rollback-probe").is_some(),
            "valid catalog should load"
        );
        std::fs::remove_file(&path).unwrap();
        load_global_pricing_catalog(tmp.path());
        assert!(
            global_pricing_rates("acme/rollback-probe").is_none(),
            "removing pricing.json must clear stale rates on same-process reload"
        );
    }

    fn catalog_from(json: &str) -> GlobalPricingCatalog {
        let mut c: GlobalPricingCatalog = serde_json::from_str(json).unwrap();
        c.finalize();
        c
    }

    #[test]
    fn exact_case_insensitive_match_resolves_component_rates() {
        let c = catalog_from(
            r#"{"models":{"gemini-3.1-pro-preview":{"input_usd_per_mtok":1.25,"output_usd_per_mtok":10.0}}}"#,
        );
        assert_eq!(
            c.rates_for("gemini-3.1-pro-preview"),
            Some((1.25, 10.0, 0.0))
        );
        assert_eq!(
            c.rates_for("Gemini-3.1-Pro-Preview"),
            Some((1.25, 10.0, 0.0))
        );
    }

    #[test]
    fn blended_rate_fills_both_dimensions() {
        let c = catalog_from(r#"{"models":{"some-cloud-model":{"usd_per_mtok":2.0}}}"#);
        assert_eq!(c.rates_for("some-cloud-model"), Some((2.0, 2.0, 0.0)));
    }

    #[test]
    fn provider_qualified_ids_absent_from_catalog_stay_unpriced() {
        // A public catalog prices a bare `gpt-oss-120b`, but a self-hosted
        // provider serves it under a provider-qualified id — exact-only matching
        // must NOT misprice the free self-hosted variant.
        let c = catalog_from(r#"{"models":{"gpt-oss-120b":{"output_usd_per_mtok":0.5}}}"#);
        assert_eq!(c.rates_for("selfhost/gpt-oss-120b"), None);
        assert_eq!(c.rates_for("myorg/gpt-oss-120b"), None);
    }

    #[test]
    fn unpriced_entries_are_not_indexed() {
        let c = catalog_from(r#"{"models":{"free-model":{"usd_per_mtok":0.0}}}"#);
        assert_eq!(c.rates_for("free-model"), None);
        assert_eq!(c.priced_len(), 0);
    }

    #[test]
    fn no_fuzzy_substring_or_leaf_collapse() {
        let c = catalog_from(r#"{"models":{"grok-4":{"input_usd_per_mtok":3.0}}}"#);
        assert_eq!(
            c.rates_for("grok-4.3"),
            None,
            "must not collapse grok-4.3 onto grok-4"
        );
    }
}
