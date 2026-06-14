use super::embeddings::EmbeddingProvider;
use super::traits::{Memory, MemoryCategory, MemoryEntry, is_recent_recall_query};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::OnceCell;
use uuid::Uuid;
use zeroclaw_api::session_keys::sanitize_session_key;

/// Qdrant vector database memory backend.
///
/// Uses Qdrant's REST API for vector storage and semantic search.
/// Requires an embedding model_provider for converting text to vectors.
pub struct QdrantMemory {
    alias: String,
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
    /// * `url` - Qdrant server URL (e.g., `"http://localhost:6333"`)
    /// * `collection` - Collection name for storing memories
    /// * `api_key` - Optional API key for Qdrant Cloud
    /// * `embedder` - Embedding model_provider for vector conversion
    pub async fn new(
        alias: &str,
        url: &str,
        collection: &str,
        api_key: Option<String>,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Result<Self> {
        let mem = Self::new_lazy(alias, url, collection, api_key, embedder);

        // Ensure collection exists with correct schema
        mem.ensure_collection().await?;
        if mem.embedder.dimensions() > 0 {
            mem.migrate_session_ids_to_sanitized().await?;
            zeroclaw_config::schema::v2::migrate_qdrant_collection_to_v3(
                &mem.client,
                &mem.base_url,
                &mem.collection,
                mem.api_key.as_deref(),
            )
            .await?;
        }
        mem.initialized.set(()).ok();

        Ok(mem)
    }

    /// Create a Qdrant memory backend with lazy initialization.
    ///
    /// Collection will be created on first operation. Use this when calling
    /// from a synchronous context (e.g., the memory factory).
    pub fn new_lazy(
        alias: &str,
        url: &str,
        collection: &str,
        api_key: Option<String>,
        embedder: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        let base_url = url.trim_end_matches('/').to_string();
        let client = zeroclaw_config::schema::build_runtime_proxy_client("memory.qdrant");

        Self {
            alias: alias.to_string(),
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
                if self.embedder.dimensions() > 0 {
                    self.migrate_session_ids_to_sanitized().await?;
                    zeroclaw_config::schema::v2::migrate_qdrant_collection_to_v3(
                        &self.client,
                        &self.base_url,
                        &self.collection,
                        self.api_key.as_deref(),
                    )
                    .await?;
                }
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

    /// Scroll all points whose payload `agent_id` is on the supplied
    /// allowlist, optionally filtered by category and session_id.
    /// Used by `recall_for_agents`'s recent/time-only branch and the
    /// embedding-empty fallback so the agent_id check happens at the
    /// query boundary, not after a broader fetch.
    async fn list_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.ensure_initialized().await?;

        let mut must_conditions: Vec<serde_json::Value> = Vec::new();
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
        must_conditions.push(serde_json::json!({
            "key": "agent_id",
            "match": { "any": allowed_agent_ids }
        }));

        let scroll_body = serde_json::json!({
            "limit": 1000,
            "with_payload": true,
            "filter": { "must": must_conditions }
        });

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/scroll", self.collection),
            )
            .json(&scroll_body)
            .send()
            .await
            .context("failed to scroll Qdrant for allowed agent set")?;

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
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                    agent_alias: payload.agent_id.clone(),
                    agent_id: payload.agent_id,
                })
            })
            .collect();

        Ok(entries)
    }

    async fn ensure_collection(&self) -> Result<()> {
        let dims = self.embedder.dimensions();
        if dims == 0 {
            // Noop embedder — skip vector collection setup
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
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

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Created Qdrant collection '{}' with {} dimensions",
                self.collection, dims
            )
        );

        Ok(())
    }

    /// One-shot, idempotent normalization of `payload.session_id`.
    ///
    /// Mirrors the SQLite-backed migration: rewrite rows that were persisted
    /// before the orchestrator sanitized session keys at the source so the
    /// new sanitized recall filter still matches them. Iterates the
    /// collection with a paginated scroll, gathers distinct `session_id`
    /// values, and issues one `set payload` per (old → new) pair where the
    /// sanitized form differs from the stored one.
    async fn migrate_session_ids_to_sanitized(&self) -> Result<()> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut next_offset: Option<serde_json::Value> = None;

        loop {
            let mut scroll_body = serde_json::json!({
                "limit": 1000,
                "with_payload": true,
                "with_vector": false,
            });
            if let Some(ref offset) = next_offset {
                scroll_body["offset"] = offset.clone();
            }

            let resp = self
                .request(
                    reqwest::Method::POST,
                    &format!("/collections/{}/points/scroll", self.collection),
                )
                .json(&scroll_body)
                .send()
                .await
                .context("failed to scroll Qdrant for session_id migration")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Qdrant scroll failed during migration ({status}): {text}");
            }

            let page: QdrantScrollResult = resp.json().await?;
            for point in &page.result.points {
                if let Some(ref payload) = point.payload
                    && let Some(ref sid) = payload.session_id
                {
                    seen.insert(sid.clone());
                }
            }

            match page.result.next_page_offset {
                Some(offset) if !offset.is_null() => next_offset = Some(offset),
                _ => break,
            }
        }

        let mut rewritten = 0usize;
        for old in &seen {
            let new = sanitize_session_key(old);
            if new == *old {
                continue;
            }

            let body = serde_json::json!({
                "payload": { "session_id": new },
                "filter": {
                    "must": [{
                        "key": "session_id",
                        "match": { "value": old }
                    }]
                }
            });

            let resp = self
                .request(
                    reqwest::Method::POST,
                    &format!("/collections/{}/points/payload", self.collection),
                )
                .query(&[("wait", "true")])
                .json(&body)
                .send()
                .await
                .context("failed to set payload during Qdrant session_id migration")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Qdrant set payload failed during migration ({status}): {text}");
            }

            rewritten += 1;
        }

        if rewritten > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"rewritten": rewritten, "collection": self.collection})
                    ),
                "Normalized session_id payload values in Qdrant collection to sanitized form"
            );
        }

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

    /// Build a Qdrant `must` payload filter from `(field, value)` pairs.
    fn must_filter(fields: &[(&str, &str)]) -> serde_json::Value {
        let must: Vec<serde_json::Value> = fields
            .iter()
            .map(|(field, value)| serde_json::json!({"key": field, "match": {"value": value}}))
            .collect();
        serde_json::json!({"must": must})
    }

    /// Scroll for the first point matching every `(field, value)` filter
    /// pair, decoded into a `MemoryEntry`. Returns `None` when nothing
    /// matches.
    async fn scroll_first_matching(&self, fields: &[(&str, &str)]) -> Result<Option<MemoryEntry>> {
        self.ensure_initialized().await?;

        let scroll_body = serde_json::json!({
            "filter": Self::must_filter(fields),
            "limit": 1,
            "with_payload": true,
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
                namespace: "default".into(),
                importance: None,
                superseded_by: None,
                agent_alias: payload.agent_id.clone(),
                agent_id: payload.agent_id,
            })
        });
        Ok(entry)
    }

    /// Delete every point matching every `(field, value)` filter pair.
    /// Qdrant's delete response does not expose a per-call match count,
    /// so this returns `true` on success regardless of how many points
    /// were touched.
    async fn delete_points_matching(&self, fields: &[(&str, &str)]) -> Result<bool> {
        self.ensure_initialized().await?;

        let delete_body = serde_json::json!({"filter": Self::must_filter(fields)});
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

        Ok(true)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
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
    #[serde(default)]
    next_page_offset: Option<serde_json::Value>,
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
        self.store_with_agent(key, content, category, session_id, None, None, None)
            .await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        if is_recent_recall_query(query) {
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

        let mut entries: Vec<MemoryEntry> = result
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
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                    agent_alias: payload.agent_id.clone(),
                    agent_id: payload.agent_id,
                })
            })
            .collect();

        // Filter by time range if specified
        if let Some(s) = since {
            entries.retain(|e| e.timestamp.as_str() >= s);
        }
        if let Some(u) = until {
            entries.retain(|e| e.timestamp.as_str() <= u);
        }

        Ok(entries)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        self.scroll_first_matching(&[("key", key)]).await
    }

    async fn get_for_agent(&self, key: &str, agent_id: &str) -> Result<Option<MemoryEntry>> {
        self.scroll_first_matching(&[("key", key), ("agent_id", agent_id)])
            .await
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
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                    agent_alias: payload.agent_id.clone(),
                    agent_id: payload.agent_id,
                })
            })
            .collect();

        Ok(entries)
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        self.delete_points_matching(&[("key", key)]).await
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> Result<bool> {
        // Qdrant's delete response does not expose a match count, so
        // probe for a matching point first. Returning `false` when
        // nothing exists keeps the bool meaningful for callers (absent
        // and deleted are distinguishable).
        if self
            .scroll_first_matching(&[("key", key), ("agent_id", agent_id)])
            .await?
            .is_none()
        {
            return Ok(false);
        }
        self.delete_points_matching(&[("key", key), ("agent_id", agent_id)])
            .await
    }

    async fn purge_session_for_agent(&self, session_id: &str, agent_id: &str) -> Result<usize> {
        let matches = self
            .list(None, Some(session_id))
            .await?
            .into_iter()
            .filter(|entry| entry.agent_id.as_deref() == Some(agent_id))
            .count();
        if matches == 0 {
            return Ok(0);
        }
        self.delete_points_matching(&[("session_id", session_id), ("agent_id", agent_id)])
            .await?;
        Ok(matches)
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

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        self.ensure_initialized().await?;

        let combined_text = format!("{}\n{}", key, content);
        let embedding = self.embedder.embed_one(&combined_text).await?;
        if embedding.is_empty() {
            anyhow::bail!("Qdrant requires non-zero dimensional embeddings");
        }

        let id = Uuid::new_v4().to_string();
        let timestamp = Utc::now().to_rfc3339();

        // Attribute un-scoped writes to the synthesized `default`
        // agent so cross-agent recall's `must agent_id IN (...)` filter
        // never sees a payload-less point as globally visible. Qdrant
        // uses alias verbatim as agent_id (no UUID indirection at the
        // storage layer; see `Memory::ensure_agent_uuid` default impl).
        let resolved_agent_id = agent_id.unwrap_or("default").to_string();
        let payload = MemoryPayload {
            key: key.to_string(),
            content: content.to_string(),
            category: Self::category_to_str(&category),
            timestamp,
            session_id: session_id.map(str::to_string),
            agent_id: Some(resolved_agent_id.clone()),
        };

        // Pre-upsert cleanup must scope to the writing agent so sibling
        // points under the same key for other agents survive.
        // Propagate failures so a cleanup error doesn't leave duplicate
        // (agent_id, key) points after the upsert lands.
        self.delete_points_matching(&[("key", key), ("agent_id", resolved_agent_id.as_str())])
            .await
            .context("qdrant pre-upsert cleanup failed")?;

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

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Empty allowlist = no agent filter (matches the wrapper's
        // semantics; see the SQL backends).
        if allowed_agent_ids.is_empty() {
            return self.recall(query, limit, session_id, since, until).await;
        }

        // Recent/time-only branch: scroll with a payload `must` filter
        // on `agent_id` so unattributed points never reach the caller.
        if is_recent_recall_query(query) {
            let mut entries = self
                .list_for_agents(allowed_agent_ids, None, session_id)
                .await?;
            if let Some(s) = since {
                entries.retain(|e| e.timestamp.as_str() >= s);
            }
            if let Some(u) = until {
                entries.retain(|e| e.timestamp.as_str() <= u);
            }
            entries.truncate(limit);
            return Ok(entries);
        }

        self.ensure_initialized().await?;

        let embedding = self.embedder.embed_one(query).await?;
        if embedding.is_empty() {
            // No embedding available: fall back to listing under the
            // allowlist. Same surface as `recall`'s fallback.
            return self
                .list_for_agents(allowed_agent_ids, None, session_id)
                .await;
        }

        // Build a `must` filter that combines the optional session_id
        // with the agent_id allowlist. The agent_id filter lives in
        // the search call, not in a post-fetch scroll: legacy points
        // whose payload lacks `agent_id` are simply not returned (the
        // V3 store path attributes everything to `default` if no agent
        // is in scope, so no payload should be agent_id-less after
        // upgrade).
        let mut must: Vec<serde_json::Value> = Vec::new();
        if let Some(sid) = session_id {
            must.push(serde_json::json!({
                "key": "session_id",
                "match": { "value": sid }
            }));
        }
        must.push(serde_json::json!({
            "key": "agent_id",
            "match": { "any": allowed_agent_ids }
        }));

        let search_body = serde_json::json!({
            "vector": embedding,
            "limit": limit,
            "with_payload": true,
            "filter": { "must": must }
        });

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/search", self.collection),
            )
            .json(&search_body)
            .send()
            .await
            .context("failed to search Qdrant for allowed agent set")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant search failed ({status}): {text}");
        }

        let result: QdrantSearchResult = resp.json().await?;

        let mut entries: Vec<MemoryEntry> = result
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
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                    agent_alias: payload.agent_id.clone(),
                    agent_id: payload.agent_id,
                })
            })
            .collect();

        if let Some(s) = since {
            entries.retain(|e| e.timestamp.as_str() >= s);
        }
        if let Some(u) = until {
            entries.retain(|e| e.timestamp.as_str() <= u);
        }
        Ok(entries)
    }
}

impl ::zeroclaw_api::attribution::Attributable for QdrantMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::Qdrant)
    }
    fn alias(&self) -> &str {
        &self.alias
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
            agent_id: None,
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
            agent_id: None,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("session_id"));
        assert!(!json.contains("agent_id"));
    }
}
