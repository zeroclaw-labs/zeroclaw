//! Unauthenticated cross-provider model catalog via models.dev.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::pricing::{ModelRates, sane_mtok};
use anyhow::Result;
use serde::Deserialize;
use tokio::sync::OnceCell;

const CATALOG_URL: &str = "https://models.dev/api.json";
const FETCH_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Deserialize)]
pub(crate) struct ProviderEntry {
    #[serde(default)]
    models: HashMap<String, ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    cost: Option<ModelCost>,
    #[serde(default)]
    limit: Option<ModelLimit>,
}

/// models.dev `cost` block: USD per 1M tokens (the same unit ZeroClaw's rate
/// sheet uses, so no conversion is needed).
#[derive(Debug, Deserialize, Clone, Copy, Default)]
struct ModelCost {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
}

/// models.dev `limit` block: `context` is the model's maximum input window in
/// tokens, the same unit `providers.models.<type>.<alias>.context_window` uses.
#[derive(Debug, Deserialize, Clone, Copy, Default)]
struct ModelLimit {
    #[serde(default)]
    context: Option<u64>,
}

pub(crate) type Catalog = HashMap<String, ProviderEntry>;

static CACHED_CATALOG: OnceCell<Arc<Catalog>> = OnceCell::const_new();

/// Fetch and parse the models.dev catalog fresh (no process cache). Used by the
/// live-pricing refresher so its fallback tracks upstream changes per cycle;
/// the cached [`list_models_for`] path stays on the process-lifetime cache.
pub(crate) async fn fetch_catalog() -> Result<Arc<Catalog>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()?;
    let response = client.get(CATALOG_URL).send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    Ok(Arc::new(parse_catalog(&bytes)?))
}

/// Parse the models.dev JSON into the in-memory `Catalog` shape. Pure
/// function — unit tests construct minimal JSON byte slices and assert
/// the filter logic without any network call.
pub(crate) fn parse_catalog(bytes: &[u8]) -> Result<Catalog> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Filter a parsed catalog for a model_provider key. Sorted, deduped.
/// Pure — separated from the live fetch so it can be unit-tested.
pub(crate) fn filter_models(catalog: &Catalog, provider_key: &str) -> Result<Vec<String>> {
    let entry = catalog.get(provider_key).ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"model_provider": provider_key})),
            "models_dev: provider not in catalog"
        );
        anyhow::Error::msg(format!(
            "model_provider {provider_key:?} is not in the models.dev catalog"
        ))
    })?;
    let mut ids: Vec<String> = entry.models.values().map(|m| m.id.clone()).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub async fn list_models_for(provider_key: &str) -> Result<Vec<String>> {
    ::zeroclaw_log::scope!(
        model_provider_type: "models_dev",
        model_provider_alias: "catalog",
        => async move {
            let catalog = CACHED_CATALOG.get_or_try_init(fetch_catalog).await?;
            filter_models(catalog, provider_key)
        }
    )
    .await
}

/// Same listing as [`list_models_for`], each id paired with the context window
/// the catalog publishes for it. `None` means the catalog has no `limit.context`
/// for that model — callers surface that as "unknown", never as a default.
pub async fn list_models_with_context_for(
    provider_key: &str,
) -> Result<Vec<(String, Option<usize>)>> {
    ::zeroclaw_log::scope!(
        model_provider_type: "models_dev",
        model_provider_alias: "catalog",
        => async move {
            let catalog = CACHED_CATALOG.get_or_try_init(fetch_catalog).await?;
            let ids = filter_models(catalog, provider_key)?;
            let windows = context_windows_from_catalog(catalog, provider_key);
            Ok(ids
                .into_iter()
                .map(|id| {
                    let ctx = windows.get(&id).copied();
                    (id, ctx)
                })
                .collect())
        }
    )
    .await
}

pub(crate) fn pricing_from_catalog(
    catalog: &Catalog,
    provider_key: &str,
) -> HashMap<String, ModelRates> {
    let mut out = HashMap::new();
    let Some(entry) = catalog.get(provider_key) else {
        return out;
    };
    for model in entry.models.values() {
        let Some(cost) = model.cost else { continue };
        // models.dev `cost` is already USD per 1M tokens, no scaling. Each
        // dimension is sanity-bounded so a malformed catalog entry can't bill
        // an absurd cost (same ceiling as the gateway path).
        let rates = ModelRates {
            input_per_mtok: cost.input.and_then(sane_mtok),
            output_per_mtok: cost.output.and_then(sane_mtok),
            cached_input_per_mtok: cost.cache_read.and_then(sane_mtok),
        };
        if !rates.is_empty() {
            out.insert(model.id.clone(), rates);
        }
    }
    out
}

/// Largest context window treated as plausible. Frontier models are at 1M–10M
/// tokens; anything past this is a malformed catalog entry, not a real limit.
const MAX_PLAUSIBLE_CONTEXT_WINDOW: u64 = 100_000_000;

/// Reject zero and absurd values so a malformed catalog entry can't widen an
/// agent's trim budget past anything the model could actually accept.
fn sane_context_window(raw: u64) -> Option<usize> {
    if raw == 0 || raw > MAX_PLAUSIBLE_CONTEXT_WINDOW {
        return None;
    }
    usize::try_from(raw).ok()
}

/// Per-model context windows for a provider, in tokens. Mirrors
/// [`pricing_from_catalog`]: a view materialized from the catalog on demand,
/// never stored. Models with no `limit.context` are absent from the map.
pub(crate) fn context_windows_from_catalog(
    catalog: &Catalog,
    provider_key: &str,
) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    let Some(entry) = catalog.get(provider_key) else {
        return out;
    };
    for model in entry.models.values() {
        if let Some(ctx) = model
            .limit
            .and_then(|l| l.context)
            .and_then(sane_context_window)
        {
            out.insert(model.id.clone(), ctx);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_CATALOG: &str = r#"{
        "anthropic": {
            "models": {
                "claude-sonnet-4-6": {"id": "claude-sonnet-4-6"},
                "claude-opus-4-7":   {"id": "claude-opus-4-7"}
            }
        },
        "xai": {
            "models": {
                "grok-4.3":     {"id": "grok-4.3"},
                "grok-2-vision":{"id": "grok-2-vision"}
            }
        },
        "empty": { "models": {} }
    }"#;

    #[test]
    fn parses_catalog_with_typical_shape() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).expect("parses");
        assert_eq!(catalog.len(), 3);
        assert!(catalog.contains_key("anthropic"));
        assert!(catalog.contains_key("xai"));
    }

    #[test]
    fn filter_returns_sorted_ids() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let ids = filter_models(&catalog, "xai").unwrap();
        assert_eq!(ids, vec!["grok-2-vision", "grok-4.3"]);
    }

    #[test]
    fn filter_dedups() {
        // Models.dev model_id values could in theory collide; the filter
        // dedups the output list so the menu doesn't render duplicates.
        let raw = r#"{"x": {"models": {"a": {"id": "m1"}, "b": {"id": "m1"}}}}"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let ids = filter_models(&catalog, "x").unwrap();
        assert_eq!(ids, vec!["m1"]);
    }

    #[test]
    fn filter_returns_empty_for_empty_entry() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let ids = filter_models(&catalog, "empty").unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn filter_errors_on_unknown_key() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let err = filter_models(&catalog, "missing").expect_err("must error");
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn parse_errors_on_malformed_json() {
        assert!(parse_catalog(b"not json").is_err());
    }

    #[test]
    fn pricing_from_catalog_reads_cost_and_skips_unpriced() {
        // `cost` is USD per 1M tokens; models without it are omitted.
        let raw = r#"{
            "kilo": {
                "models": {
                    "a": {"id": "minimax-m2.7", "cost": {"input": 0.3, "output": 1.2, "cache_read": 0.06}},
                    "b": {"id": "no-cost-model"}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let map = pricing_from_catalog(&catalog, "kilo");
        let m = map.get("minimax-m2.7").expect("priced");
        assert_eq!(m.input_per_mtok, Some(0.3));
        assert_eq!(m.output_per_mtok, Some(1.2));
        assert_eq!(m.cached_input_per_mtok, Some(0.06));
        assert!(!map.contains_key("no-cost-model"));
        // Unknown provider key yields an empty map, not an error.
        assert!(pricing_from_catalog(&catalog, "absent").is_empty());
    }

    #[test]
    fn context_windows_from_catalog_reads_limit_and_skips_unlimited() {
        // `limit.context` is the max input window in tokens; models without a
        // `limit` block are omitted rather than defaulted.
        let raw = r#"{
            "anthropic": {
                "models": {
                    "a": {"id": "claude-opus-4-8", "limit": {"context": 1000000, "output": 128000}},
                    "b": {"id": "claude-opus-4-5", "limit": {"context": 200000}},
                    "c": {"id": "no-limit-model"},
                    "d": {"id": "empty-limit-model", "limit": {}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let map = context_windows_from_catalog(&catalog, "anthropic");
        assert_eq!(map.get("claude-opus-4-8"), Some(&1_000_000));
        assert_eq!(map.get("claude-opus-4-5"), Some(&200_000));
        assert!(!map.contains_key("no-limit-model"));
        assert!(!map.contains_key("empty-limit-model"));
        // Unknown provider key yields an empty map, not an error.
        assert!(context_windows_from_catalog(&catalog, "absent").is_empty());
    }

    #[test]
    fn context_windows_reject_zero_and_absurd_values() {
        // A malformed entry must not widen an agent's trim budget.
        let raw = r#"{
            "p": {
                "models": {
                    "a": {"id": "zero", "limit": {"context": 0}},
                    "b": {"id": "absurd", "limit": {"context": 999999999999}},
                    "c": {"id": "ok", "limit": {"context": 8192}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let map = context_windows_from_catalog(&catalog, "p");
        assert!(!map.contains_key("zero"));
        assert!(!map.contains_key("absurd"));
        assert_eq!(map.get("ok"), Some(&8192));
    }

    #[test]
    fn unknown_limit_fields_do_not_break_parsing() {
        // models.dev adds fields over time; parsing must tolerate them so a
        // catalog change can't break model listing.
        let raw = r#"{
            "p": { "models": { "a": {"id": "m", "limit": {"context": 4096, "future_field": 7}} } }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        assert_eq!(
            context_windows_from_catalog(&catalog, "p").get("m"),
            Some(&4096)
        );
    }
}
