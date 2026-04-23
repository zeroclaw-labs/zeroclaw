//! Live model catalog client.
//!
//! Fetches the provider's `/v1/models` endpoint and resolves semantic tiers
//! (chat / thinking / fast) from a YAML file. Cached for 60 seconds per
//! process so the agent can switch models mid-conversation without
//! re-fetching on every call.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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
        let bytes = tokio::fs::read(&self.tiers_path)
            .await
            .with_context(|| format!("reading tier config at {}", self.tiers_path.display()))?;
        let parsed: TiersFile = serde_yaml::from_slice(&bytes)
            .with_context(|| format!("parsing YAML at {}", self.tiers_path.display()))?;
        Ok(parsed.tiers)
    }

    /// Returns the provider key (e.g. `custom:http://adi-cliproxy.internal:8317/v1`)
    /// that callers should use when staging a model switch that targets this
    /// catalog's provider. The key preserves the `/v1` suffix because the
    /// production config expects the full base URL to follow `custom:`.
    pub fn provider_key(&self) -> String {
        let trimmed = self.base_url.trim_end_matches('/');
        format!("custom:{trimmed}")
    }

    pub async fn resolve_tier(&self, tier: &str) -> Result<String> {
        let tiers = self.list_tiers().await?;
        tiers
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(tier))
            .map(|t| t.model.clone())
            .ok_or_else(|| {
                let available: Vec<&str> = tiers.iter().map(|t| t.name.as_str()).collect();
                anyhow::anyhow!(
                    "unknown tier '{tier}'. Available tiers: {}",
                    available.join(", ")
                )
            })
    }
}
