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
#[cfg_attr(not(test), allow(dead_code))] // `modalities` is parsed and exposed via `model_supports_vision`, which only has test callers until #8733 capability-routing lands.
struct ModelEntry {
    id: String,
    #[serde(default)]
    cost: Option<ModelCost>,
    /// models.dev `modalities` block. Carries the per-model `input` and
    /// `output` modality lists (e.g. `input: ["text", "image"]`). Previously
    /// dropped during deserialization; per-model vision support is now
    /// resolved through this field. See #8733.
    #[serde(default)]
    modalities: Option<Modalities>,
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

/// models.dev `modalities` block — only the `input` dimension is consumed
/// today. Membership of `"image"` in `input` is what callers use to decide
/// whether a model can accept vision attachments; `output` (and any future
/// modality vectors we do not yet read) are tolerated by `serde` defaults
/// rather than deserialized into named fields.
#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
struct Modalities {
    #[serde(default)]
    input: Vec<String>,
}

impl Modalities {
    /// Whether this model advertises image input support. Conservative: only
    /// an explicit `"image"` token in `input` flips it on. Malformed
    /// catalog entries (missing `modalities` or empty `input`) yield
    /// `false`; callers fall back to the family default in that case.
    #[cfg_attr(not(test), allow(dead_code))] // only test callers until #8733 capability-routing lands
    fn supports_image_input(&self) -> bool {
        self.input.iter().any(|m| m == "image")
    }
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

/// Per-model vision support resolved from the parsed catalog.
///
/// Returns `Some(true)` when the model is in the catalog and its
/// `modalities.input` lists `"image"`. Returns `Some(false)` when the model
/// is in the catalog but does not advertise image input. Returns `None`
/// when the model isn't in the catalog, the provider key isn't, or the
/// catalog entry has no `modalities` block at all — callers should fall
/// back to the family default in that case.
///
/// Pure / sync / no network. This is the parser half of #8733; wiring the
/// result into `provider.capabilities()` and the orchestrator
/// `supports_vision()` call site is a separate change tracked on #8733.
#[cfg_attr(not(test), allow(dead_code))] // only test callers until #8733 capability-routing lands
pub(crate) fn model_supports_vision(
    catalog: &Catalog,
    provider_key: &str,
    model_id: &str,
) -> Option<bool> {
    let entry = catalog.get(provider_key)?;
    let model = entry.models.get(model_id)?;
    Some(model.modalities.as_ref()?.supports_image_input())
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
    fn model_supports_vision_reads_modalities_input_image() {
        // models.dev `modalities.input` advertises "image" for vision models
        // and is absent for text-only models. The helper must read that field
        // and return Some(bool) for cataloged models.
        let raw = r#"{
            "xai": {
                "models": {
                    "grok-2-vision": {"id": "grok-2-vision",
                                      "modalities": {"input": ["text", "image"], "output": ["text"]}},
                    "grok-4.3":     {"id": "grok-4.3",
                                      "modalities": {"input": ["text"], "output": ["text"]}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        assert_eq!(
            model_supports_vision(&catalog, "xai", "grok-2-vision"),
            Some(true)
        );
        assert_eq!(
            model_supports_vision(&catalog, "xai", "grok-4.3"),
            Some(false)
        );
    }

    #[test]
    fn model_supports_vision_returns_none_for_missing_modalities_block() {
        // Old-shape entries (no `modalities` block) must yield None, not
        // false — callers fall back to the family default in that case.
        let raw = r#"{
            "xai": {
                "models": {
                    "grok-4.3": {"id": "grok-4.3"}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        assert_eq!(model_supports_vision(&catalog, "xai", "grok-4.3"), None);
    }

    #[test]
    fn model_supports_vision_returns_none_for_unknown_provider_or_model() {
        let raw = r#"{
            "xai": {
                "models": {
                    "grok-2-vision": {"id": "grok-2-vision",
                                      "modalities": {"input": ["text", "image"], "output": ["text"]}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        // Unknown model id within a known provider.
        assert_eq!(model_supports_vision(&catalog, "xai", "grok-99"), None);
        // Unknown provider key.
        assert_eq!(
            model_supports_vision(&catalog, "absent", "grok-2-vision"),
            None
        );
    }

    #[test]
    fn model_supports_vision_does_not_match_non_image_modality_aliases() {
        // Defensive: only an exact "image" token in `input` flips vision on.
        // "images" (plural) and "image_url" (a wire-format alias used in
        // OpenAI's request shape) must NOT count — they are not what
        // models.dev emits and a future schema drift should surface as a
        // false negative, not a silent true.
        let raw = r#"{
            "fake": {
                "models": {
                    "alias-1": {"id": "alias-1",
                                "modalities": {"input": ["text", "images"], "output": ["text"]}},
                    "alias-2": {"id": "alias-2",
                                "modalities": {"input": ["text", "image_url"], "output": ["text"]}}
                }
            }
        }"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        assert_eq!(
            model_supports_vision(&catalog, "fake", "alias-1"),
            Some(false)
        );
        assert_eq!(
            model_supports_vision(&catalog, "fake", "alias-2"),
            Some(false)
        );
    }
}
