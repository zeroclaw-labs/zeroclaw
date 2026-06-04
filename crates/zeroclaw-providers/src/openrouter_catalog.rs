//! Cross-vendor model catalog via OpenRouter's public `/api/v1/models` endpoint.
//!
//! Fallback for compat providers that don't have a `models.dev` entry and
//! can't reach their native `/models` endpoint without a credential. Each
//! OpenRouter model id is `<vendor>/<slug>`; we filter by vendor prefix
//! (e.g. `x-ai/` for xAI, `tencent/` for Hunyuan) and return the slug list.
//!
//! Cached once per process (`OnceCell`) and shared across all callers.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use tokio::sync::OnceCell;
use zeroclaw_api::model_provider::ModelPricing;

const CATALOG_URL: &str = "https://openrouter.ai/api/v1/models";
const FETCH_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Deserialize)]
struct CatalogResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize, Clone)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    pricing: Option<ModelPricing>,
}

/// Flat catalog — model IDs only (used by `list_models`).
static CACHED_CATALOG: OnceCell<Arc<Vec<String>>> = OnceCell::const_new();
/// Enriched catalog — model IDs with pricing (used by `list_models_with_pricing`).
static CACHED_CATALOG_WITH_PRICING: OnceCell<Arc<Vec<ModelEntryWithPricing>>> =
    OnceCell::const_new();

#[derive(Clone)]
struct ModelEntryWithPricing {
    id: String,
    pricing: Option<ModelPricing>,
}

async fn fetch_catalog() -> Result<Arc<Vec<String>>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()?;
    let response = client.get(CATALOG_URL).send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    Ok(Arc::new(parse_catalog(&bytes)?))
}

async fn fetch_catalog_with_pricing() -> Result<Arc<Vec<ModelEntryWithPricing>>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()?;
    let response = client.get(CATALOG_URL).send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    Ok(Arc::new(parse_catalog_with_pricing(&bytes)?))
}

/// Parse the OpenRouter JSON into a flat list of model ids. Pure — unit
/// tests construct minimal JSON byte slices and assert filter logic
/// without any network call.
pub(crate) fn parse_catalog(bytes: &[u8]) -> Result<Vec<String>> {
    let body: CatalogResponse = serde_json::from_slice(bytes)?;
    Ok(body.data.into_iter().map(|m| m.id).collect())
}

/// Parse the OpenRouter JSON into a list of model entries with pricing.
fn parse_catalog_with_pricing(bytes: &[u8]) -> Result<Vec<ModelEntryWithPricing>> {
    let body: CatalogResponse = serde_json::from_slice(bytes)?;
    Ok(body
        .data
        .into_iter()
        .map(|m| ModelEntryWithPricing {
            id: m.id,
            pricing: m.pricing,
        })
        .collect())
}

/// Filter a parsed catalog by vendor prefix, returning the slug portion of
/// each match. Sorted and deduped. Errors if nothing matches. Pure —
/// separated from the live fetch so it can be unit-tested.
pub(crate) fn filter_by_vendor(catalog: &[String], vendor_prefix: &str) -> Result<Vec<String>> {
    let needle = format!("{vendor_prefix}/");
    let mut slugs: Vec<String> = catalog
        .iter()
        .filter_map(|id| id.strip_prefix(&needle).map(ToString::to_string))
        .collect();
    if slugs.is_empty() {
        anyhow::bail!("OpenRouter catalog has no entries under vendor prefix {vendor_prefix:?}");
    }
    slugs.sort();
    slugs.dedup();
    Ok(slugs)
}

/// Filter an enriched catalog by vendor prefix, returning model entries with
/// pricing. Sorted and deduped by id.
fn filter_by_vendor_with_pricing(
    catalog: &[ModelEntryWithPricing],
    vendor_prefix: &str,
) -> Result<Vec<zeroclaw_api::model_provider::ModelInfo>> {
    use zeroclaw_api::model_provider::ModelInfo;
    let needle = format!("{vendor_prefix}/");
    let mut models: Vec<ModelInfo> = catalog
        .iter()
        .filter_map(|e| {
            e.id.strip_prefix(&needle).map(|slug| ModelInfo {
                id: slug.to_string(),
                pricing: e.pricing.clone(),
            })
        })
        .collect();
    if models.is_empty() {
        anyhow::bail!("OpenRouter catalog has no entries under vendor prefix {vendor_prefix:?}");
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

/// Return the slug portion of every OpenRouter model id whose vendor prefix
/// matches `vendor_prefix`. The vendor prefix is the segment before `/` in
/// the id (e.g. `x-ai`, `tencent`, `rekaai`). The returned slugs are sorted
/// and deduplicated.
pub async fn list_models_for_vendor(vendor_prefix: &str) -> Result<Vec<String>> {
    let catalog = CACHED_CATALOG.get_or_try_init(fetch_catalog).await?;
    filter_by_vendor(catalog, vendor_prefix)
}

/// Return model entries with pricing for every OpenRouter model id whose
/// vendor prefix matches `vendor_prefix`. Sorted and deduplicated by id.
pub async fn list_models_for_vendor_with_pricing(
    vendor_prefix: &str,
) -> Result<Vec<zeroclaw_api::model_provider::ModelInfo>> {
    let catalog = CACHED_CATALOG_WITH_PRICING
        .get_or_try_init(fetch_catalog_with_pricing)
        .await?;
    filter_by_vendor_with_pricing(catalog, vendor_prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_CATALOG: &str = r#"{
        "data": [
            {"id": "x-ai/grok-4.3"},
            {"id": "x-ai/grok-2-vision"},
            {"id": "anthropic/claude-sonnet-4-6"},
            {"id": "tencent/hunyuan-t1"},
            {"id": "tencent/hunyuan-turbos"}
        ]
    }"#;

    #[test]
    fn parses_catalog_into_flat_id_list() {
        let ids = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        assert_eq!(ids.len(), 5);
        assert!(ids.contains(&"x-ai/grok-4.3".to_string()));
    }

    #[test]
    fn filter_strips_vendor_prefix() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let slugs = filter_by_vendor(&catalog, "x-ai").unwrap();
        assert_eq!(slugs, vec!["grok-2-vision", "grok-4.3"]);
    }

    #[test]
    fn filter_handles_multi_match() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let slugs = filter_by_vendor(&catalog, "tencent").unwrap();
        assert_eq!(slugs, vec!["hunyuan-t1", "hunyuan-turbos"]);
    }

    #[test]
    fn filter_errors_when_no_match() {
        let catalog = parse_catalog(TINY_CATALOG.as_bytes()).unwrap();
        let err = filter_by_vendor(&catalog, "missing").expect_err("must error");
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn filter_dedups() {
        // OpenRouter could (theoretically) list the same model id twice;
        // dedup keeps the picker clean.
        let raw = r#"{"data": [{"id":"v/m"},{"id":"v/m"},{"id":"v/n"}]}"#;
        let catalog = parse_catalog(raw.as_bytes()).unwrap();
        let slugs = filter_by_vendor(&catalog, "v").unwrap();
        assert_eq!(slugs, vec!["m", "n"]);
    }

    #[test]
    fn parse_errors_on_malformed_json() {
        assert!(parse_catalog(b"not json").is_err());
    }
}
