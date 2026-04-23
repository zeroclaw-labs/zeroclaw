//! Live model catalog client.
//!
//! Fetches the provider's `/v1/models` endpoint and resolves semantic tiers
//! (chat / thinking / fast) from a YAML file. Cached for 60 seconds per
//! process so the agent can switch models mid-conversation without
//! re-fetching on every call.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const CATALOG_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub owned_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierEntry {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct TiersFile {
    tiers: Vec<TierEntry>,
}

#[derive(Debug, Default)]
struct CachedCatalog {
    models: Option<(Vec<ModelEntry>, Instant)>,
}

pub struct ModelCatalogClient {
    base_url: String,
    api_key: String,
    tiers_path: PathBuf,
    http: reqwest::Client,
    cache: Arc<Mutex<CachedCatalog>>,
}

impl ModelCatalogClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, tiers_path: impl Into<PathBuf>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .context("building catalog HTTP client")?;
        Ok(Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            tiers_path: tiers_path.into(),
            http,
            cache: Arc::new(Mutex::new(CachedCatalog::default())),
        })
    }

    pub async fn list_models(&self) -> Result<Vec<ModelEntry>> {
        {
            let cache = self.cache.lock().await;
            if let Some((models, fetched_at)) = &cache.models {
                if fetched_at.elapsed() < CATALOG_TTL {
                    return Ok(models.clone());
                }
            }
        }

        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("model catalog fetch failed: status={status} body={body}");
        }

        let parsed: ModelsResponse = resp
            .json()
            .await
            .context("parsing /v1/models response")?;

        {
            let mut cache = self.cache.lock().await;
            cache.models = Some((parsed.data.clone(), Instant::now()));
        }

        Ok(parsed.data)
    }

    pub async fn list_tiers(&self) -> Result<Vec<TierEntry>> {
        anyhow::bail!("not yet implemented")
    }

    pub async fn resolve_tier(&self, _tier: &str) -> Result<String> {
        anyhow::bail!("not yet implemented")
    }
}
