use super::embeddings::EmbeddingProvider;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::OnceCell;
use uuid::Uuid;

/// Qdrant vector database memory backend.
///
/// Uses Qdrant's REST API for vector storage and semantic search.
/// Requires an embedding provider for converting text to vectors.
pub struct QdrantMemory {
    client: reqwest::Client,
    base_url: String,
    collection: String,
    api_key: Option<String>,
    embedder: Arc<dyn EmbeddingProvider>,
    /// Tracks whether collection has been initialized (lazy init for sync factory).
    initialized: OnceCell<()>,
}

impl QdrantMemory {
    /// Create a new Qdrant memory backend.
    ///
    /// # Arguments
    /// * `url` - Qdrant server URL (e.g., "http://localhost:6333")
    /// * `collection` - Collection name for storing memories
    /// * `api_key` - Optional API key for Qdrant Cloud
    /// * `embedder` - Embedding provider for vector conversion
    pub async fn new(
        url: &str,
        collection: &str,
        api_key: Option<String>,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Result<Self> {
        let mem = Self::new_lazy(url, collection, api_key, embedder);

        // Ensure collection exists with correct schema
        mem.ensure_collection().await?;
        mem.initialized.set(()).ok();

        Ok(mem)
    }

    /// Create a Qdrant memory backend with lazy initialization.
    ///
    /// Collection will be created on first operation. Use this when calling
    /// from a synchronous context (e.g., the memory factory).
    pub fn new_lazy(
        url: &str,
        collection: &str,
        api_key: Option<String>,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        let base_url = url.trim_end_matches('/').to_string();
        let client = crate::config::build_runtime_proxy_client("memory.qdrant");

        Self {
            client,
            base_url,
            collection: collection.to_string(),
            api_key,
            embedder,
            initialized: OnceCell::new(),
        }
    }

    /// Ensure the collection is initialized (called lazily on first operation).
    async fn ensure_initialized(&self) -> Result<()> {
        self.initialized
            .get_or_try_init(|| async {
                self.ensure_collection().await?;
                Ok::<(), anyhow::Error>(())
            })
            .await?;
        Ok(())
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.request(method, &url);

        if let Some(ref key) = self.api_key {
            req = req.header("api-key", key);
        }

        req.header("Content-Type", "application/json")
    }

    async fn ensure_collection(&self) -> Result<()> {
        let dims = self.embedder.dimensions();
        if dims == 0 {
            // Noop embedder — skip vector collection setup
            tracing::warn!(
                "Qdrant memory using noop embedder (0 dimensions); vector search disabled"
            );
            return Ok(());
        }

        // Check if collection exists
        let resp = self
            .request(
                reqwest::Method::GET,
                &format!("/collections/{}", self.collection),
            )
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                // Collection exists
                return Ok(());
            }
            Ok(r) if r.status().as_u16() == 404 => {
                // Collection doesn't exist, create it
            }
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                anyhow::bail!("Qdrant collection check failed ({status}): {text}");
            }
            Err(e) => {
                anyhow::bail!("Qdrant connection failed: {e}");
            }
        }

        // Create collection with vector config
        let create_body = serde_json::json!({
            "vectors": {
                "size": dims,
                "distance": "Cosine"
            }
        });

        let resp = self
            .request(
                reqwest::Method::PUT,
                &format!("/collections/{}", self.collection),
            )
            .json(&create_body)
            .send()
            .await
            .context("failed to create Qdrant collection")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant collection creation failed ({status}): {text}");
        }

        tracing::info!(
            "Created Qdrant collection '{}' with {} dimensions",
            self.collection,
            dims
        );

        Ok(())
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

/// Qdrant point payload structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryPayload {
    key: String,
    content: String,
    category: String,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

/// Qdrant search result
#[derive(Debug, Deserialize)]
struct QdrantSearchResult {
    result: Vec<QdrantScoredPoint>,
}

#[derive(Debug, Deserialize)]
struct QdrantScoredPoint {
    id: serde_json::Value,
    score: f64,
    payload: Option<MemoryPayload>,
}

/// Qdrant scroll result
#[derive(Debug, Deserialize)]
struct QdrantScrollResult {
    result: QdrantScrollPoints,
}

#[derive(Debug, Deserialize)]
struct QdrantScrollPoints {
    points: Vec<QdrantPoint>,
}

#[derive(Debug, Deserialize)]
struct QdrantPoint {
    id: serde_json::Value,
    payload: Option<MemoryPayload>,
}

#[async_trait]
impl Memory for QdrantMemory {
    fn name(&self) -> &str {
        "qdrant"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.ensure_initialized().await?;

        // Generate embedding for the content
        let combined_text = format!("{}\n{}", key, content);
        let embedding = self.embedder.embed_one(&combined_text).await?;

        if embedding.is_empty() {
            anyhow::bail!("Qdrant requires non-zero dimensional embeddings");
        }

        let id = Uuid::new_v4().to_string();
        let timestamp = Utc::now().to_rfc3339();

        let payload = MemoryPayload {
            key: key.to_string(),
            content: content.to_string(),
            category: Self::category_to_str(&category),
            timestamp,
            session_id: session_id.map(str::to_string),
        };

        // Delete any existing point with the same key first
        let _ = self.forget(key).await;

        // Upsert point
        let upsert_body = serde_json::json!({
            "points": [{
                "id": id,
                "vector": embedding,
                "payload": payload
            }]
        });

        let resp = self
            .request(
                reqwest::Method::PUT,
                &format!("/collections/{}/points", self.collection),
            )
            .query(&[("wait", "true")])
            .json(&upsert_body)
            .send()
            .await
            .context("failed to upsert point to Qdrant")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant upsert failed ({status}): {text}");
        }

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        if query.trim().is_empty() {
            return self.list(None, session_id).await;
        }

        self.ensure_initialized().await?;

        // Generate embedding for the query
        let embedding = self.embedder.embed_one(query).await?;

        if embedding.is_empty() {
            // Fallback to listing if embeddings aren't available
            return self.list(None, session_id).await;
        }

        // Build filter for session_id if provided
        let filter = session_id.map(|sid| {
            serde_json::json!({
                "must": [{
                    "key": "session_id",
                    "match": { "value": sid }
                }]
            })
        });

        let mut search_body = serde_json::json!({
            "vector": embedding,
            "limit": limit,
            "with_payload": true
        });

        if let Some(f) = filter {
            search_body["filter"] = f;
        }

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/search", self.collection),
            )
            .json(&search_body)
            .send()
            .await
            .context("failed to search Qdrant")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant search failed ({status}): {text}");
        }

        let result: QdrantSearchResult = resp.json().await?;

        let entries = result
            .result
            .into_iter()
            .filter_map(|point| {
                let payload = point.payload?;
                let id = match &point.id {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };

                Some(MemoryEntry {
                    id,
                    key: payload.key,
                    content: payload.content,
                    category: Self::parse_category(&payload.category),
                    timestamp: payload.timestamp,
                    session_id: payload.session_id,
                    score: Some(point.score),
                })
            })
            .collect();

        Ok(entries)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        self.ensure_initialized().await?;

        // Scroll with filter for exact key match
        let scroll_body = serde_json::json!({
            "filter": {
                "must": [{
                    "key": "key",
                    "match": { "value": key }
                }]
            },
            "limit": 1,
            "with_payload": true
        });

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/scroll", self.collection),
            )
            .json(&scroll_body)
            .send()
            .await
            .context("failed to scroll Qdrant")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant scroll failed ({status}): {text}");
        }

        let result: QdrantScrollResult = resp.json().await?;

        let entry = result.result.points.into_iter().next().and_then(|point| {
            let payload = point.payload?;
            let id = match &point.id {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => return None,
            };

            Some(MemoryEntry {
                id,
                key: payload.key,
                content: payload.content,
                category: Self::parse_category(&payload.category),
                timestamp: payload.timestamp,
                session_id: payload.session_id,
                score: None,
            })
        });

        Ok(entry)
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.ensure_initialized().await?;

        // Build filter conditions
        let mut must_conditions = Vec::new();

        if let Some(cat) = category {
            must_conditions.push(serde_json::json!({
                "key": "category",
                "match": { "value": Self::category_to_str(cat) }
            }));
        }

        if let Some(sid) = session_id {
            must_conditions.push(serde_json::json!({
                "key": "session_id",
                "match": { "value": sid }
            }));
        }

        let mut scroll_body = serde_json::json!({
            "limit": 1000,
            "with_payload": true
        });

        if !must_conditions.is_empty() {
            scroll_body["filter"] = serde_json::json!({ "must": must_conditions });
        }

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/scroll", self.collection),
            )
            .json(&scroll_body)
            .send()
            .await
            .context("failed to scroll Qdrant")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant scroll failed ({status}): {text}");
        }

        let result: QdrantScrollResult = resp.json().await?;

        let entries = result
            .result
            .points
            .into_iter()
            .filter_map(|point| {
                let payload = point.payload?;
                let id = match &point.id {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };

                Some(MemoryEntry {
                    id,
                    key: payload.key,
                    content: payload.content,
                    category: Self::parse_category(&payload.category),
                    timestamp: payload.timestamp,
                    session_id: payload.session_id,
                    score: None,
                })
            })
            .collect();

        Ok(entries)
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        self.ensure_initialized().await?;

        // Delete points matching the key
        let delete_body = serde_json::json!({
            "filter": {
                "must": [{
                    "key": "key",
                    "match": { "value": key }
                }]
            }
        });

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/delete", self.collection),
            )
            .query(&[("wait", "true")])
            .json(&delete_body)
            .send()
            .await
            .context("failed to delete from Qdrant")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant delete failed ({status}): {text}");
        }

        // Qdrant doesn't return deleted count easily, assume success
        Ok(true)
    }

    async fn count(&self) -> Result<usize> {
        self.ensure_initialized().await?;

        let resp = self
            .request(
                reqwest::Method::GET,
                &format!("/collections/{}", self.collection),
            )
            .send()
            .await
            .context("failed to get Qdrant collection info")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant collection info failed ({status}): {text}");
        }

        let json: serde_json::Value = resp.json().await?;

        let count = json
            .get("result")
            .and_then(|r| r.get("points_count"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0);

        let count =
            usize::try_from(count).context("Qdrant returned a points count that exceeds usize")?;
        Ok(count)
    }

    async fn health_check(&self) -> bool {
        let resp = self.request(reqwest::Method::GET, "/").send().await;

        matches!(resp, Ok(r) if r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_to_str_maps_known_categories() {
        assert_eq!(QdrantMemory::category_to_str(&MemoryCategory::Core), "core");
        assert_eq!(
            QdrantMemory::category_to_str(&MemoryCategory::Daily),
            "daily"
        );
        assert_eq!(
            QdrantMemory::category_to_str(&MemoryCategory::Conversation),
            "conversation"
        );
        assert_eq!(
            QdrantMemory::category_to_str(&MemoryCategory::Custom("notes".into())),
            "notes"
        );
    }

    #[test]
    fn parse_category_maps_known_and_custom_values() {
        assert_eq!(QdrantMemory::parse_category("core"), MemoryCategory::Core);
        assert_eq!(QdrantMemory::parse_category("daily"), MemoryCategory::Daily);
        assert_eq!(
            QdrantMemory::parse_category("conversation"),
            MemoryCategory::Conversation
        );
        assert_eq!(
            QdrantMemory::parse_category("custom_notes"),
            MemoryCategory::Custom("custom_notes".into())
        );
    }

    #[test]
    fn memory_payload_serializes_correctly() {
        let payload = MemoryPayload {
            key: "test_key".into(),
            content: "test content".into(),
            category: "core".into(),
            timestamp: "2026-02-20T00:00:00Z".into(),
            session_id: Some("session-1".into()),
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("test_key"));
        assert!(json.contains("test content"));
        assert!(json.contains("session-1"));
    }

    #[test]
    fn memory_payload_skips_none_session_id() {
        let payload = MemoryPayload {
            key: "test_key".into(),
            content: "test content".into(),
            category: "core".into(),
            timestamp: "2026-02-20T00:00:00Z".into(),
            session_id: None,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("session_id"));
    }
}
