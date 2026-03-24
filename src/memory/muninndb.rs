use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// MuninnDB cognitive memory backend.
///
/// Connects to a running MuninnDB instance via its REST API.
/// MuninnDB provides semantic search with Hebbian reinforcement,
/// Ebbinghaus decay, and associative recall — all handled server-side.
/// No local embedder required.
pub struct MuninndbMemory {
    client: reqwest::Client,
    base_url: String,
    vault: String,
    api_key: Option<String>,
}

// ── MuninnDB API types ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct WriteRequest {
    concept: String,
    content: String,
    vault: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct WriteResponse {
    id: String,
}

#[derive(Serialize)]
struct ActivateRequest {
    context: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    threshold: Option<f32>,
    max_results: usize,
    vault: String,
}

#[derive(Deserialize)]
struct ActivateResponse {
    #[serde(default)]
    activations: Vec<ActivationItem>,
}

#[derive(Deserialize)]
struct ActivationItem {
    id: String,
    #[serde(default)]
    concept: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    score: f64,
}

#[derive(Deserialize)]
struct ReadResponse {
    id: String,
    #[serde(default)]
    concept: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct ListEngramsResponse {
    #[serde(default)]
    engrams: Vec<ReadResponse>,
    #[serde(default)]
    total: usize,
}

#[derive(Deserialize)]
struct StatsResponse {
    #[serde(default)]
    total_engrams: usize,
}

impl MuninndbMemory {
    pub fn new(url: &str, vault: &str, api_key: Option<String>) -> Self {
        let base_url = url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            client,
            base_url,
            vault: vault.to_string(),
            api_key,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.request(method, &url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    /// Map a ZeroClaw MemoryCategory to a MuninnDB tag.
    fn category_tag(category: &MemoryCategory) -> String {
        format!("zc:{category}")
    }

    /// Convert a MuninnDB engram to a ZeroClaw MemoryEntry.
    fn to_entry(engram: &ReadResponse, score: Option<f64>) -> MemoryEntry {
        let category = engram
            .tags
            .iter()
            .find(|t| t.starts_with("zc:"))
            .map(|t| t.strip_prefix("zc:").unwrap_or("core"))
            .unwrap_or("core");

        let session_id = engram
            .tags
            .iter()
            .find(|t| t.starts_with("session:"))
            .map(|t| t.strip_prefix("session:").unwrap_or("").to_string());

        let ts = if engram.created_at > 0 {
            chrono::DateTime::from_timestamp_nanos(engram.created_at)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        } else {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };

        MemoryEntry {
            id: engram.id.clone(),
            key: engram.concept.clone(),
            content: engram.content.clone(),
            category: serde_json::from_value(serde_json::Value::String(category.to_string()))
                .unwrap_or(MemoryCategory::Core),
            timestamp: ts,
            session_id,
            score,
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
        }
    }
}

#[async_trait]
impl Memory for MuninndbMemory {
    fn name(&self) -> &str {
        "muninndb"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let mut tags = vec![Self::category_tag(&category)];
        if let Some(sid) = session_id {
            tags.push(format!("session:{sid}"));
        }

        let body = WriteRequest {
            concept: key.to_string(),
            content: content.to_string(),
            vault: self.vault.clone(),
            tags,
        };

        let resp = self
            .request(reqwest::Method::POST, "/api/engrams")
            .json(&body)
            .send()
            .await
            .context("muninndb: failed to store engram")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("muninndb store failed ({status}): {body}");
        }

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let body = ActivateRequest {
            context: vec![query.to_string()],
            threshold: Some(0.3),
            max_results: limit,
            vault: self.vault.clone(),
        };

        let resp = self
            .request(reqwest::Method::POST, "/api/activate")
            .json(&body)
            .send()
            .await
            .context("muninndb: failed to activate")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("muninndb recall failed ({status}): {body}");
        }

        let result: ActivateResponse = resp.json().await?;

        // Fetch full engram data for each activation to get tags/timestamps
        let mut entries = Vec::with_capacity(result.activations.len());
        for item in &result.activations {
            let engram = ReadResponse {
                id: item.id.clone(),
                concept: item.concept.clone(),
                content: item.content.clone(),
                created_at: 0,
                tags: vec![],
            };
            entries.push(Self::to_entry(&engram, Some(item.score)));
        }

        Ok(entries)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        // MuninnDB uses IDs, not keys. Try to find by concept via activate.
        let body = ActivateRequest {
            context: vec![key.to_string()],
            threshold: Some(0.8),
            max_results: 1,
            vault: self.vault.clone(),
        };

        let resp = self
            .request(reqwest::Method::POST, "/api/activate")
            .json(&body)
            .send()
            .await
            .context("muninndb: failed to get engram")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let result: ActivateResponse = resp.json().await?;

        if let Some(item) = result.activations.first() {
            if item.concept == key {
                let engram = ReadResponse {
                    id: item.id.clone(),
                    concept: item.concept.clone(),
                    content: item.content.clone(),
                    created_at: 0,
                    tags: vec![],
                };
                return Ok(Some(Self::to_entry(&engram, Some(item.score))));
            }
        }

        Ok(None)
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let path = format!("/api/engrams?vault={}&limit=200", self.vault);

        let resp = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .context("muninndb: failed to list engrams")?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let result: ListEngramsResponse = resp.json().await?;
        let entries: Vec<MemoryEntry> = result
            .engrams
            .iter()
            .map(|e| Self::to_entry(e, None))
            .collect();

        Ok(entries)
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        // Try as ID first (MuninnDB uses ULIDs)
        let path = format!("/api/engrams/{}?vault={}", key, self.vault);
        let resp = self
            .request(reqwest::Method::DELETE, &path)
            .send()
            .await
            .context("muninndb: failed to forget engram")?;

        Ok(resp.status().is_success())
    }

    async fn count(&self) -> Result<usize> {
        let path = format!("/api/stats?vault={}", self.vault);
        let resp = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .context("muninndb: failed to get stats")?;

        if !resp.status().is_success() {
            return Ok(0);
        }

        let stats: StatsResponse = resp.json().await.unwrap_or(StatsResponse { total_engrams: 0 });
        Ok(stats.total_engrams)
    }

    async fn health_check(&self) -> bool {
        self.request(reqwest::Method::GET, "/api/health")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_tag_formatting() {
        assert_eq!(
            MuninndbMemory::category_tag(&MemoryCategory::Core),
            "zc:core"
        );
        assert_eq!(
            MuninndbMemory::category_tag(&MemoryCategory::Daily),
            "zc:daily"
        );
        assert_eq!(
            MuninndbMemory::category_tag(&MemoryCategory::Custom("project".into())),
            "zc:project"
        );
    }

    #[test]
    fn to_entry_extracts_category_from_tags() {
        let engram = ReadResponse {
            id: "01ABC".into(),
            concept: "test-key".into(),
            content: "test content".into(),
            created_at: 0,
            tags: vec!["zc:daily".into(), "session:s1".into()],
        };

        let entry = MuninndbMemory::to_entry(&engram, Some(0.9));
        assert_eq!(entry.key, "test-key");
        assert_eq!(entry.category, MemoryCategory::Daily);
        assert_eq!(entry.session_id.as_deref(), Some("s1"));
        assert_eq!(entry.score, Some(0.9));
    }
}
