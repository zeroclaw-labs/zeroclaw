use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::Context;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Lore Context memory backend.
///
/// Uses the Lore Context REST API for persistent, semantically-indexed memory.
/// See: <https://github.com/Lore-Context/lore-context>
///
/// Configuration:
/// - Base URL from `[memory.lore_context].url` or `LORE_API_URL` env var
/// - API key from `[memory.lore_context].api_key` or `LORE_API_KEY` env var
pub struct LoreContextMemory {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl LoreContextMemory {
    /// Create a new Lore Context memory backend.
    ///
    /// # Arguments
    /// * `url` - Lore Context API base URL (e.g. `"http://localhost:3000"`)
    /// * `api_key` - Optional API key for authentication
    pub fn new(url: &str, api_key: Option<String>) -> Self {
        let base_url = url.trim_end_matches('/').to_string();
        let client = zeroclaw_config::schema::build_runtime_proxy_client("memory.lore_context");

        Self {
            client,
            base_url,
            api_key,
        }
    }

    /// Build a request with auth header.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.request(method, &url);

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        req.header("Content-Type", "application/json")
    }

    fn category_to_str(category: &MemoryCategory) -> String {
        match category {
            MemoryCategory::Core => "core".to_string(),
            MemoryCategory::Daily => "daily".to_string(),
            MemoryCategory::Conversation => "conversation".to_string(),
            MemoryCategory::Custom(name) => name.clone(),
        }
    }

    fn parse_category(value: &str) -> MemoryCategory {
        match value {
            "core" => MemoryCategory::Core,
            "daily" => MemoryCategory::Daily,
            "conversation" => MemoryCategory::Conversation,
            other => MemoryCategory::Custom(other.to_string()),
        }
    }
}

/// Lore API response for a single memory entry.
#[derive(Debug, Clone, Deserialize)]
struct LoreMemoryEntry {
    id: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    content: String,
    #[serde(default = "default_category")]
    category: String,
    #[serde(default, alias = "created_at")]
    timestamp: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default = "default_namespace")]
    namespace: String,
    #[serde(default)]
    importance: Option<f64>,
    #[serde(default)]
    superseded_by: Option<String>,
}

fn default_category() -> String {
    "core".into()
}

fn default_namespace() -> String {
    "default".into()
}

/// Lore API response for list/search operations.
#[derive(Debug, Deserialize)]
struct LoreListResponse {
    #[serde(default)]
    entries: Vec<LoreMemoryEntry>,
    #[serde(default)]
    total: Option<usize>,
}

/// Lore API write request body.
#[derive(Debug, Serialize)]
struct LoreWriteRequest {
    key: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

/// Lore API search request body.
#[derive(Debug, Serialize)]
struct LoreSearchRequest {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

/// Lore API forget request body.
#[derive(Debug, Serialize)]
struct LoreForgetRequest {
    key: String,
}

fn lore_entry_to_memory_entry(entry: LoreMemoryEntry) -> MemoryEntry {
    MemoryEntry {
        id: entry.id,
        key: entry.key,
        content: entry.content,
        category: LoreContextMemory::parse_category(&entry.category),
        timestamp: if entry.timestamp.is_empty() {
            chrono::Utc::now().to_rfc3339()
        } else {
            entry.timestamp
        },
        session_id: entry.session_id,
        score: entry.score,
        namespace: entry.namespace,
        importance: entry.importance,
        superseded_by: entry.superseded_by,
    }
}

#[async_trait]
impl Memory for LoreContextMemory {
    fn name(&self) -> &str {
        "lore_context"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let body = LoreWriteRequest {
            key: key.to_string(),
            content: content.to_string(),
            category: Some(Self::category_to_str(&category)),
            session_id: session_id.map(str::to_string),
        };

        let resp = self
            .request(reqwest::Method::POST, "/v1/memory/write")
            .json(&body)
            .send()
            .await
            .context("failed to write memory to Lore Context")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lore Context write failed ({status}): {text}");
        }

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // If query is empty, delegate to list + time filter
        if query.trim().is_empty() {
            let mut entries = self.list(None, session_id).await?;
            if let Some(s) = since {
                entries.retain(|e| e.timestamp.as_str() >= s);
            }
            if let Some(u) = until {
                entries.retain(|e| e.timestamp.as_str() <= u);
            }
            entries.truncate(limit);
            return Ok(entries);
        }

        let body = LoreSearchRequest {
            query: query.to_string(),
            limit: Some(limit),
        };

        let resp = self
            .request(reqwest::Method::POST, "/v1/memory/search")
            .json(&body)
            .send()
            .await
            .context("failed to search Lore Context")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lore Context search failed ({status}): {text}");
        }

        let result: LoreListResponse = resp
            .json()
            .await
            .context("failed to parse Lore Context search response")?;

        let mut entries: Vec<MemoryEntry> =
            result.entries.into_iter().map(lore_entry_to_memory_entry).collect();

        // Filter by session_id if provided
        if let Some(sid) = session_id {
            entries.retain(|e| e.session_id.as_deref() == Some(sid));
        }

        // Filter by time range if specified
        if let Some(s) = since {
            entries.retain(|e| e.timestamp.as_str() >= s);
        }
        if let Some(u) = until {
            entries.retain(|e| e.timestamp.as_str() <= u);
        }

        Ok(entries)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let encoded = urlencoding::encode(key);
        let path = format!("/v1/memory/{encoded}");

        let resp = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .context("failed to get memory from Lore Context")?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lore Context get failed ({status}): {text}");
        }

        let entry: LoreMemoryEntry = resp
            .json()
            .await
            .context("failed to parse Lore Context get response")?;

        Ok(Some(lore_entry_to_memory_entry(entry)))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let mut path = "/v1/memory/list".to_string();
        let mut params = Vec::new();

        if let Some(cat) = category {
            params.push(("category", Self::category_to_str(cat)));
        }
        if let Some(sid) = session_id {
            params.push(("session_id", sid.to_string()));
        }

        if !params.is_empty() {
            let query: String = params
                .iter()
                .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            path = format!("{path}?{query}");
        }

        let resp = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .context("failed to list memories from Lore Context")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lore Context list failed ({status}): {text}");
        }

        let result: LoreListResponse = resp
            .json()
            .await
            .context("failed to parse Lore Context list response")?;

        Ok(result
            .entries
            .into_iter()
            .map(lore_entry_to_memory_entry)
            .collect())
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let body = LoreForgetRequest {
            key: key.to_string(),
        };

        let resp = self
            .request(reqwest::Method::POST, "/v1/memory/forget")
            .json(&body)
            .send()
            .await
            .context("failed to forget memory in Lore Context")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lore Context forget failed ({status}): {text}");
        }

        Ok(true)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let resp = self
            .request(reqwest::Method::GET, "/v1/memory/list")
            .send()
            .await
            .context("failed to count memories in Lore Context")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Lore Context count failed ({status}): {text}");
        }

        let result: LoreListResponse = resp
            .json()
            .await
            .context("failed to parse Lore Context count response")?;

        Ok(result.total.unwrap_or(result.entries.len()))
    }

    async fn health_check(&self) -> bool {
        let resp = self
            .request(reqwest::Method::GET, "/v1/health")
            .send()
            .await;

        matches!(resp, Ok(r) if r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_to_str_maps_known_categories() {
        assert_eq!(
            LoreContextMemory::category_to_str(&MemoryCategory::Core),
            "core"
        );
        assert_eq!(
            LoreContextMemory::category_to_str(&MemoryCategory::Daily),
            "daily"
        );
        assert_eq!(
            LoreContextMemory::category_to_str(&MemoryCategory::Conversation),
            "conversation"
        );
        assert_eq!(
            LoreContextMemory::category_to_str(&MemoryCategory::Custom("notes".into())),
            "notes"
        );
    }

    #[test]
    fn parse_category_maps_known_and_custom_values() {
        assert_eq!(
            LoreContextMemory::parse_category("core"),
            MemoryCategory::Core
        );
        assert_eq!(
            LoreContextMemory::parse_category("daily"),
            MemoryCategory::Daily
        );
        assert_eq!(
            LoreContextMemory::parse_category("conversation"),
            MemoryCategory::Conversation
        );
        assert_eq!(
            LoreContextMemory::parse_category("custom_notes"),
            MemoryCategory::Custom("custom_notes".into())
        );
    }

    #[test]
    fn lore_entry_to_memory_entry_converts_fields() {
        let entry = LoreMemoryEntry {
            id: "id-1".into(),
            key: "favorite_language".into(),
            content: "Rust".into(),
            category: "core".into(),
            timestamp: "2026-04-01T00:00:00Z".into(),
            session_id: Some("sess-1".into()),
            score: Some(0.95),
            namespace: "default".into(),
            importance: Some(0.8),
            superseded_by: None,
        };

        let mem = lore_entry_to_memory_entry(entry);
        assert_eq!(mem.id, "id-1");
        assert_eq!(mem.key, "favorite_language");
        assert_eq!(mem.content, "Rust");
        assert_eq!(mem.category, MemoryCategory::Core);
        assert_eq!(mem.session_id.as_deref(), Some("sess-1"));
        assert_eq!(mem.score, Some(0.95));
        assert_eq!(mem.namespace, "default");
        assert_eq!(mem.importance, Some(0.8));
        assert!(mem.superseded_by.is_none());
    }

    #[test]
    fn lore_entry_to_memory_entry_fills_empty_timestamp() {
        let entry = LoreMemoryEntry {
            id: "id-2".into(),
            key: "test".into(),
            content: "value".into(),
            category: "core".into(),
            timestamp: String::new(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
        };

        let mem = lore_entry_to_memory_entry(entry);
        assert!(!mem.timestamp.is_empty());
        assert!(mem.timestamp.contains('T'));
    }

    #[test]
    fn new_strips_trailing_slash() {
        let mem = LoreContextMemory::new("http://localhost:3000/", None);
        assert_eq!(mem.base_url, "http://localhost:3000");
    }

    #[test]
    fn name_returns_lore_context() {
        let mem = LoreContextMemory::new("http://localhost:3000", None);
        assert_eq!(mem.name(), "lore_context");
    }
}
