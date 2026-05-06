//! Unauthenticated cross-model_provider model catalog via models.dev.
//!
//! `https://models.dev/api.json` is a community-maintained public aggregator
//! that lists model IDs for 100+ model_providers (Anthropic, OpenAI, Google,
//! Bedrock, Azure, Moonshot, Qwen, …). No API key required, same shape for
//! every model_provider, updated frequently (verified 2026-04-21: includes
//! claude-sonnet-4-6, claude-opus-4-7, gemini-2.5-pro). We fetch the catalog
//! once per process and cache in memory.
//!
//! Providers that have a native public `/models` endpoint (OpenRouter,
//! Ollama's `/api/tags`) override `ModelProvider::list_models` directly and
//! skip this path.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use tokio::sync::OnceCell;

const CATALOG_URL: &str = "https://models.dev/api.json";
const FETCH_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Deserialize)]
struct ProviderEntry {
    #[serde(default)]
    models: HashMap<String, ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

type Catalog = Arc<HashMap<String, ProviderEntry>>;

static CATALOG: OnceCell<Catalog> = OnceCell::const_new();

async fn fetch_catalog() -> Result<Catalog> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()?;
    let response = client.get(CATALOG_URL).send().await?.error_for_status()?;
    let catalog: HashMap<String, ProviderEntry> = response.json().await?;
    Ok(Arc::new(catalog))
}

/// Look up model IDs for a model_provider, keyed by `models.dev`'s model_provider name.
///
/// First call fetches the catalog; subsequent calls hit the cache. The
/// returned list is sorted for stable menu rendering.
pub async fn list_models_for(provider_key: &str) -> Result<Vec<String>> {
    let catalog = CATALOG.get_or_try_init(fetch_catalog).await?;
    let entry = catalog.get(provider_key).ok_or_else(|| {
        anyhow::anyhow!("model_provider {provider_key:?} is not in the models.dev catalog")
    })?;
    let mut ids: Vec<String> = entry.models.values().map(|m| m.id.clone()).collect();
    ids.sort();
    Ok(ids)
}
