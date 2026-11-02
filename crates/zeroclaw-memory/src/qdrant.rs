use super::embeddings::EmbeddingProvider;
use super::traits::{Memory, MemoryCategory, MemoryEntry, is_recent_recall_query};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::OnceCell;
use uuid::Uuid;
use zeroclaw_api::session_keys::sanitize_session_key;

/// Qdrant vector database memory backend.
/// Uses Qdrant's REST API for vector storage and semantic search.
/// Requires an embedding model_provider for converting text to vectors.
pub struct QdrantMemory {
    alias: String,
    client: reqwest::Client,
    base_url: String,
    collection: String,
    api_key: Option<String>,
    // Behind an `RwLock` so `config/set` can hot-swap the embedder on a
    // long-lived handle after a provider-profile change Reads snapshot
    // the `Arc` and drop the guard before any `.await`.
    embedder: RwLock<Arc<dyn EmbeddingProvider>>,
    /// Tracks whether collection has been initialized (lazy init for sync factory).
    initialized: OnceCell<()>,
    /// `[storage.qdrant.<alias>].hybrid`: create a missing collection with a
    /// `bm25` sparse vector filled by Qdrant's server-side BM25 inference.
    hybrid: bool,
    /// The collection carries the `bm25` sparse vector (created that way, or
    /// found with it). Only then are points written with it and recall run
    /// as dense + BM25.
    hybrid_active: AtomicBool,
    /// `memory.vector_weight` / `memory.keyword_weight`, the same blend the
    /// SQLite backend applies, so hybrid scores stay on its 0-1 scale.
    vector_weight: f32,
    keyword_weight: f32,
}

/// Sparse vector holding the server-side BM25 representation of a point's
/// key and content (the same text that is embedded).
const BM25_VECTOR: &str = "bm25";
/// Qdrant's built-in BM25 inference model (Qdrant >= 1.15).
const BM25_MODEL: &str = "qdrant/bm25";

impl QdrantMemory {
    fn add_time_range_filter(
        must: &mut Vec<serde_json::Value>,
        since: Option<&str>,
        until: Option<&str>,
    ) {
        let mut range = serde_json::Map::new();
        if let Some(value) = since {
            range.insert("gte".into(), serde_json::Value::String(value.into()));
        }
        if let Some(value) = until {
            range.insert("lte".into(), serde_json::Value::String(value.into()));
        }
        if !range.is_empty() {
            must.push(serde_json::json!({
                "key": "timestamp",
                "range": range,
            }));
        }
    }

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
        if mem.embedder.read().dimensions() > 0 {
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
            embedder: RwLock::new(embedder),
            initialized: OnceCell::new(),
            hybrid: false,
            hybrid_active: AtomicBool::new(false),
            vector_weight: 0.7,
            keyword_weight: 0.3,
        }
    }

    /// Opt in to hybrid (dense + BM25) storage and recall for a collection this
    /// backend creates. An existing collection is used as found: hybrid when it
    /// already has the `bm25` sparse vector, dense-only otherwise.
    #[must_use]
    pub fn with_hybrid(mut self, hybrid: bool) -> Self {
        self.hybrid = hybrid;
        self
    }

    /// Weights for merging dense and BM25 scores (`memory.vector_weight`,
    /// `memory.keyword_weight`).
    #[must_use]
    pub fn with_weights(mut self, vector_weight: f32, keyword_weight: f32) -> Self {
        self.vector_weight = vector_weight;
        self.keyword_weight = keyword_weight;
        self
    }

    /// Replace the live embedder in place Existing `Arc<dyn Memory>`
    /// holders observe the new embedder on their next embed without the handle
    /// being rebuilt. Shared by the `refresh_embedder` hook and tests.
    pub(crate) fn swap_embedder(&self, embedder: Arc<dyn EmbeddingProvider>) {
        *self.embedder.write() = embedder;
    }

    /// Dimensions of the currently-installed embedder (0 = Noop / no vectors).
    /// Cheap read-only diagnostic; lets callers confirm a live embedder refresh
    /// took effect after a `config/set` provider-profile change
    pub fn embedder_dimensions(&self) -> usize {
        self.embedder.read().dimensions()
    }

    /// Ensure the collection is initialized (called lazily on first operation).
    async fn ensure_initialized(&self) -> Result<()> {
        self.initialized
            .get_or_try_init(|| async {
                self.ensure_collection().await?;
                if self.embedder.read().dimensions() > 0 {
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

        let points = self
            .scroll_all(scroll_body, "failed to scroll Qdrant for allowed agent set")
            .await?;

        let entries = points
            .into_iter()
            .filter_map(|point| {
                let payload = point.payload?;
                let id = match &point.id {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };

                Some(MemoryEntry {
                    principal_id: None,
                    id,
                    key: payload.key,
                    content: payload.content,
                    category: Self::parse_category(&payload.category),
                    timestamp: payload.timestamp,
                    session_id: payload.session_id,
                    score: None,
                    namespace: "default".into(),
                    importance: payload.importance,
                    superseded_by: None,
                    kind: None,
                    pinned: false,
                    tenant_id: None,
                    agent_alias: payload.agent_id.clone(),
                    agent_id: payload.agent_id,
                })
            })
            .collect();

        Ok(entries)
    }

    async fn scroll_all(
        &self,
        mut scroll_body: serde_json::Value,
        context: &'static str,
    ) -> Result<Vec<QdrantPoint>> {
        let mut points = Vec::new();

        loop {
            let page = self.scroll_page(&scroll_body, context).await?;
            points.extend(page.result.points);

            match page.result.next_page_offset {
                Some(offset) if !offset.is_null() => scroll_body["offset"] = offset,
                _ => break,
            }
        }

        Ok(points)
    }

    async fn scroll_page(
        &self,
        scroll_body: &serde_json::Value,
        context: &'static str,
    ) -> Result<QdrantScrollResult> {
        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/scroll", self.collection),
            )
            .json(scroll_body)
            .send()
            .await
            .context(context)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant scroll failed ({status}): {text}");
        }

        Ok(resp.json().await?)
    }

    async fn list_limited(
        &self,
        allowed_agent_ids: Option<&[&str]>,
        session_id: Option<&str>,
        limit: usize,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        self.ensure_initialized().await?;

        let mut must_conditions: Vec<serde_json::Value> = Vec::new();
        if let Some(sid) = session_id {
            must_conditions.push(serde_json::json!({
                "key": "session_id",
                "match": { "value": sid }
            }));
        }
        if let Some(agent_ids) = allowed_agent_ids {
            must_conditions.push(serde_json::json!({
                "key": "agent_id",
                "match": { "any": agent_ids }
            }));
        }

        let page_limit = limit.min(1000);
        let mut scroll_body = serde_json::json!({
            "limit": page_limit,
            "with_payload": true
        });
        if !must_conditions.is_empty() {
            scroll_body["filter"] = serde_json::json!({ "must": must_conditions });
        }

        let mut entries = Vec::with_capacity(page_limit);
        loop {
            let page = self
                .scroll_page(&scroll_body, "failed to scroll Qdrant for bounded recall")
                .await?;

            for point in page.result.points {
                let Some(payload) = point.payload else {
                    continue;
                };
                let id = match point.id {
                    serde_json::Value::String(id) => id,
                    serde_json::Value::Number(id) => id.to_string(),
                    _ => continue,
                };
                let timestamp = payload.timestamp.clone();
                if since.is_some_and(|value| timestamp.as_str() < value)
                    || until.is_some_and(|value| timestamp.as_str() > value)
                {
                    continue;
                }

                entries.push(MemoryEntry {
                    id,
                    key: payload.key,
                    content: payload.content,
                    category: Self::parse_category(&payload.category),
                    timestamp,
                    session_id: payload.session_id,
                    score: None,
                    namespace: "default".into(),
                    importance: payload.importance,
                    superseded_by: None,
                    kind: None,
                    pinned: false,
                    tenant_id: None,
                    principal_id: None,
                    agent_alias: payload.agent_id.clone(),
                    agent_id: payload.agent_id,
                });

                if entries.len() >= limit {
                    return Ok(entries);
                }
            }

            match page.result.next_page_offset {
                Some(offset) if !offset.is_null() => scroll_body["offset"] = offset,
                _ => return Ok(entries),
            }
        }
    }

    async fn list_for_agents_limited(
        &self,
        allowed_agent_ids: &[&str],
        session_id: Option<&str>,
        limit: usize,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.list_limited(Some(allowed_agent_ids), session_id, limit, since, until)
            .await
    }

    /// Rank points for `query`: a dense cosine search, plus a BM25 sparse
    /// search when the collection is hybrid, merged with the SQLite backend's
    /// `vector::hybrid_merge` so scores keep its 0-1 scale (and its
    /// `min_relevance_score` semantics). A failed BM25 leg degrades to
    /// dense-only rather than failing recall.
    async fn ranked_points(
        &self,
        embedding: Vec<f32>,
        query: &str,
        filter: Option<serde_json::Value>,
        limit: usize,
        context: &'static str,
    ) -> Result<Vec<QdrantScoredPoint>> {
        let hybrid = self.hybrid_active.load(Ordering::Relaxed);
        // Over-fetch each leg so the merge can promote a point that only one
        // leg ranks highly.
        let candidates = if hybrid {
            limit.saturating_mul(2)
        } else {
            limit
        };

        let mut search_body = serde_json::json!({
            "vector": embedding,
            "limit": candidates,
            "with_payload": true
        });
        if let Some(f) = &filter {
            search_body["filter"] = f.clone();
        }
        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/search", self.collection),
            )
            .json(&search_body)
            .send()
            .await
            .context(context)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant search failed ({status}): {text}");
        }
        let dense: QdrantSearchResult = resp.json().await?;
        if !hybrid {
            return Ok(dense.result);
        }

        let keyword = match self.bm25_points(query, filter, candidates).await {
            Ok(points) => points,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e:#}")})),
                    "Qdrant BM25 search failed; using dense-only recall"
                );
                let mut points = dense.result;
                points.truncate(limit);
                return Ok(points);
            }
        };

        Ok(Self::merge_ranked(
            dense.result,
            keyword,
            self.vector_weight,
            self.keyword_weight,
            limit,
        ))
    }

    /// BM25 leg of hybrid recall: Qdrant embeds `query` with its built-in
    /// BM25 model and scores it against the `bm25` sparse vector.
    async fn bm25_points(
        &self,
        query: &str,
        filter: Option<serde_json::Value>,
        limit: usize,
    ) -> Result<Vec<QdrantScoredPoint>> {
        let mut body = serde_json::json!({
            "query": { "text": query, "model": BM25_MODEL },
            "using": BM25_VECTOR,
            "limit": limit,
            "with_payload": true
        });
        if let Some(f) = filter {
            body["filter"] = f;
        }
        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("/collections/{}/points/query", self.collection),
            )
            .json(&body)
            .send()
            .await
            .context("failed to run Qdrant BM25 query")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant BM25 query failed ({status}): {text}");
        }
        let result: QdrantQueryResult = resp.json().await?;
        Ok(result.result.points)
    }

    /// Merge dense and BM25 hits into one ranking via `vector::hybrid_merge`.
    fn merge_ranked(
        dense: Vec<QdrantScoredPoint>,
        keyword: Vec<QdrantScoredPoint>,
        vector_weight: f32,
        keyword_weight: f32,
        limit: usize,
    ) -> Vec<QdrantScoredPoint> {
        fn point_key(id: &serde_json::Value) -> String {
            match id {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        fn scores(points: &[QdrantScoredPoint]) -> Vec<(String, f32)> {
            points
                .iter()
                .map(|p| (point_key(&p.id), p.score as f32))
                .collect()
        }
        let merged = super::vector::hybrid_merge(
            &scores(&dense),
            &scores(&keyword),
            vector_weight,
            keyword_weight,
            limit,
        );

        let mut by_id: HashMap<String, QdrantScoredPoint> = HashMap::new();
        for point in dense.into_iter().chain(keyword) {
            by_id.entry(point_key(&point.id)).or_insert(point);
        }
        merged
            .into_iter()
            .filter_map(|r| {
                let mut point = by_id.remove(&r.id)?;
                point.score = f64::from(r.final_score);
                Some(point)
            })
            .collect()
    }

    async fn ensure_collection(&self) -> Result<()> {
        let dims = self.embedder.read().dimensions();
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
                // Collection exists: go hybrid only if it already carries the
                // BM25 sparse vector (one can't be added to existing points).
                let info: serde_json::Value = r.json().await.unwrap_or_default();
                let has_bm25 = info
                    .pointer(&format!(
                        "/result/config/params/sparse_vectors/{BM25_VECTOR}"
                    ))
                    .is_some();
                self.hybrid_active.store(has_bm25, Ordering::Relaxed);
                if self.hybrid && !has_bm25 {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"collection": self.collection})),
                        "Qdrant hybrid requested but the existing collection has no bm25 sparse vector; using dense-only recall"
                    );
                }
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
        let mut create_body = serde_json::json!({
            "vectors": {
                "size": dims,
                "distance": "Cosine"
            }
        });
        if self.hybrid {
            // IDF is applied server-side, so points only carry term frequencies.
            create_body["sparse_vectors"] = serde_json::json!({
                BM25_VECTOR: { "modifier": "idf" }
            });
        }

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
        self.hybrid_active.store(self.hybrid, Ordering::Relaxed);

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Created Qdrant collection '{}' with {} dimensions{}",
                self.collection,
                dims,
                if self.hybrid {
                    " + bm25 sparse vector"
                } else {
                    ""
                }
            )
        );

        Ok(())
    }

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
                principal_id: None,
                id,
                key: payload.key,
                content: payload.content,
                category: Self::parse_category(&payload.category),
                timestamp: payload.timestamp,
                session_id: payload.session_id,
                score: None,
                namespace: "default".into(),
                importance: payload.importance,
                superseded_by: None,
                kind: None,
                pinned: false,
                tenant_id: None,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    importance: Option<f64>,
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

/// Qdrant Query API result (`/points/query`)
#[derive(Debug, Deserialize)]
struct QdrantQueryResult {
    result: QdrantQueryPoints,
}

#[derive(Debug, Deserialize)]
struct QdrantQueryPoints {
    points: Vec<QdrantScoredPoint>,
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

    fn refresh_embedder(
        &self,
        model_provider: &str,
        api_key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) {
        // Rebuild from the freshly-resolved settings and swap in place. The
        // Qdrant collection was created for the old vector dimensions; a
        // dimension change still needs a manual reindex/collection rebuild, but
        // the live handle no longer embeds against a stale endpoint/key
        let embedder: Arc<dyn EmbeddingProvider> =
            Arc::from(super::embeddings::create_embedding_provider(
                model_provider,
                api_key,
                model,
                dimensions,
            ));
        self.swap_embedder(embedder);
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
            return self
                .list_limited(None, session_id, limit, since, until)
                .await;
        }

        self.ensure_initialized().await?;

        // Generate embedding for the query
        let embedder = self.embedder.read().clone();
        let embedding = embedder.embed_one(query).await?;

        if embedding.is_empty() {
            // Fallback to listing if embeddings aren't available
            return self
                .list_limited(None, session_id, limit, since, until)
                .await;
        }

        // Build filter for session_id if provided
        let mut must = Vec::new();
        if let Some(sid) = session_id {
            must.push(serde_json::json!({
                "key": "session_id",
                "match": { "value": sid }
            }));
        }
        Self::add_time_range_filter(&mut must, since, until);

        let filter = (!must.is_empty()).then(|| serde_json::json!({ "must": must }));
        let points = self
            .ranked_points(embedding, query, filter, limit, "failed to search Qdrant")
            .await?;

        let mut entries: Vec<MemoryEntry> = points
            .into_iter()
            .filter_map(|point| {
                let payload = point.payload?;
                let id = match &point.id {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };

                Some(MemoryEntry {
                    principal_id: None,
                    id,
                    key: payload.key,
                    content: payload.content,
                    category: Self::parse_category(&payload.category),
                    timestamp: payload.timestamp,
                    session_id: payload.session_id,
                    score: Some(point.score),
                    namespace: "default".into(),
                    importance: payload.importance,
                    superseded_by: None,
                    kind: None,
                    pinned: false,
                    tenant_id: None,
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

        let points = self
            .scroll_all(scroll_body, "failed to scroll Qdrant")
            .await?;

        let entries = points
            .into_iter()
            .filter_map(|point| {
                let payload = point.payload?;
                let id = match &point.id {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };

                Some(MemoryEntry {
                    principal_id: None,
                    id,
                    key: payload.key,
                    content: payload.content,
                    category: Self::parse_category(&payload.category),
                    timestamp: payload.timestamp,
                    session_id: payload.session_id,
                    score: None,
                    namespace: "default".into(),
                    importance: payload.importance,
                    superseded_by: None,
                    kind: None,
                    pinned: false,
                    tenant_id: None,
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

    async fn purge_agent(&self, agent_alias: &str) -> Result<usize> {
        // Qdrant stores the agent alias in the `agent_id` payload field.
        let matches = self
            .list_for_agents(&[agent_alias], None, None)
            .await?
            .len();
        if matches == 0 {
            return Ok(0);
        }
        self.delete_points_matching(&[("agent_id", agent_alias)])
            .await?;
        Ok(matches)
    }

    async fn export_agent(&self, agent_alias: &str) -> Result<Vec<MemoryEntry>> {
        self.list_for_agents(&[agent_alias], None, None).await
    }

    async fn rename_agent(&self, from: &str, to: &str) -> Result<usize> {
        // Qdrant keys memory points by the agent alias in the `agent_id`
        // payload field (no UUID indirection), so rename rewrites that field on
        // every matching point via set-payload-by-filter (mirrors the
        // `session_id` migration path). Returns the count of points re-pointed.
        self.ensure_initialized().await?;
        let matches = self.list_for_agents(&[from], None, None).await?.len();
        if matches == 0 {
            return Ok(0);
        }
        let body = serde_json::json!({
            "payload": { "agent_id": to },
            "filter": Self::must_filter(&[("agent_id", from)]),
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
            .context("failed to set payload during Qdrant agent rename")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant set payload failed during agent rename ({status}): {text}");
        }
        Ok(matches)
    }

    async fn count_agent(&self, agent_alias: &str) -> Result<usize> {
        // Qdrant keys memory points by the alias in the `agent_id` payload field,
        // so `rename_agent` re-points exactly the points `list_for_agents` returns;
        // residue is that match count.
        Ok(self
            .list_for_agents(&[agent_alias], None, None)
            .await?
            .len())
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
        importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        self.ensure_initialized().await?;

        let combined_text = format!("{}\n{}", key, content);
        let embedder = self.embedder.read().clone();
        let embedding = embedder.embed_one(&combined_text).await?;
        if embedding.is_empty() {
            anyhow::bail!("Qdrant requires non-zero dimensional embeddings");
        }

        let id = Uuid::new_v4().to_string();
        let timestamp = Utc::now().to_rfc3339();

        let resolved_agent_id = agent_id.unwrap_or("default").to_string();
        let payload = MemoryPayload {
            key: key.to_string(),
            content: content.to_string(),
            category: Self::category_to_str(&category),
            timestamp,
            session_id: session_id.map(str::to_string),
            agent_id: Some(resolved_agent_id.clone()),
            importance,
        };

        self.delete_points_matching(&[("key", key), ("agent_id", resolved_agent_id.as_str())])
            .await
            .context("qdrant pre-upsert cleanup failed")?;

        let vector = if self.hybrid_active.load(Ordering::Relaxed) {
            serde_json::json!({
                "": embedding,
                BM25_VECTOR: { "text": combined_text, "model": BM25_MODEL }
            })
        } else {
            serde_json::json!(embedding)
        };
        let upsert_body = serde_json::json!({
            "points": [{
                "id": id,
                "vector": vector,
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
            return self
                .list_for_agents_limited(allowed_agent_ids, session_id, limit, since, until)
                .await;
        }

        self.ensure_initialized().await?;

        let embedder = self.embedder.read().clone();
        let embedding = embedder.embed_one(query).await?;
        if embedding.is_empty() {
            // No embedding available: fall back to listing under the
            // allowlist. Same surface as `recall`'s fallback.
            return self
                .list_for_agents_limited(allowed_agent_ids, session_id, limit, since, until)
                .await;
        }

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
        Self::add_time_range_filter(&mut must, since, until);

        let points = self
            .ranked_points(
                embedding,
                query,
                Some(serde_json::json!({ "must": must })),
                limit,
                "failed to search Qdrant for allowed agent set",
            )
            .await?;

        let mut entries: Vec<MemoryEntry> = points
            .into_iter()
            .filter_map(|point| {
                let payload = point.payload?;
                let id = match &point.id {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };

                Some(MemoryEntry {
                    principal_id: None,
                    id,
                    key: payload.key,
                    content: payload.content,
                    category: Self::parse_category(&payload.category),
                    timestamp: payload.timestamp,
                    session_id: payload.session_id,
                    score: Some(point.score),
                    namespace: "default".into(),
                    importance: payload.importance,
                    superseded_by: None,
                    kind: None,
                    pinned: false,
                    tenant_id: None,
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

    struct EmptyVectorEmbedding;

    #[async_trait::async_trait]
    impl EmbeddingProvider for EmptyVectorEmbedding {
        fn name(&self) -> &str {
            "empty"
        }

        fn dimensions(&self) -> usize {
            0
        }

        async fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![Vec::new()])
        }
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;

        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(
                read > 0,
                "Qdrant fixture closed before the request body arrived"
            );
            request.extend_from_slice(&buffer[..read]);

            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let header = String::from_utf8_lossy(&request[..header_end]);
            let content_length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                })
                .unwrap()
                .parse::<usize>()
                .unwrap();
            if request.len() >= header_end + 4 + content_length {
                return String::from_utf8(request).unwrap();
            }
        }
    }

    fn qdrant_fixture_point(id: usize) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "payload": {
                "key": format!("key-{id}"),
                "content": format!("content-{id}"),
                "category": "daily",
                "timestamp": "2026-08-25T00:00:00Z",
                "agent_id": "alpha"
            }
        })
    }

    #[tokio::test]
    async fn list_operations_follow_qdrant_scroll_continuation() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for request_index in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                let body = request.split_once("\r\n\r\n").unwrap().1;
                let body: serde_json::Value = serde_json::from_str(body).unwrap();

                assert_eq!(
                    body["filter"]["must"].as_array().is_some_and(|must| {
                        must.iter().any(|condition| condition["key"] == "agent_id")
                    }),
                    request_index < 2,
                    "scoped and unscoped list requests must keep their filters"
                );
                let expected_offset = (request_index % 2 == 1).then(|| serde_json::json!(1000));
                assert_eq!(
                    body.get("offset"),
                    expected_offset.as_ref(),
                    "the second page must carry Qdrant's continuation offset"
                );

                let points: Vec<_> = if request_index % 2 == 0 {
                    (0..1000).map(qdrant_fixture_point).collect()
                } else {
                    vec![qdrant_fixture_point(1000)]
                };
                let next_page_offset = if request_index % 2 == 0 {
                    serde_json::json!(1000)
                } else {
                    serde_json::Value::Null
                };
                let response_body = serde_json::json!({
                    "result": {
                        "points": points,
                        "next_page_offset": next_page_offset
                    }
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                )
                .unwrap();
            }
        });

        let mem = QdrantMemory::new_lazy(
            "test",
            &endpoint,
            "mem",
            None,
            Arc::new(super::super::embeddings::NoopEmbedding),
        );
        let scoped = mem.list_for_agents(&["alpha"], None, None).await.unwrap();
        let unscoped = Memory::list(&mem, None, None).await.unwrap();
        server.join().unwrap();

        assert_eq!(scoped.len(), 1001);
        assert_eq!(unscoped.len(), 1001);
    }

    #[tokio::test]
    async fn scoped_recent_and_fallback_recall_do_not_follow_pages_after_limit() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                let body = request.split_once("\r\n\r\n").unwrap().1;
                let body: serde_json::Value = serde_json::from_str(body).unwrap();

                assert_eq!(body["limit"], 2);
                assert!(body["filter"]["must"].as_array().is_some_and(|must| {
                    must.iter().any(|condition| condition["key"] == "agent_id")
                }));

                let response_body = serde_json::json!({
                    "result": {
                        "points": [qdrant_fixture_point(0), qdrant_fixture_point(1)],
                        "next_page_offset": 2
                    }
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                )
                .unwrap();
            }
        });

        let mem = QdrantMemory::new_lazy(
            "test",
            &endpoint,
            "mem",
            None,
            Arc::new(EmptyVectorEmbedding),
        );
        let recent = Memory::recall_for_agents(&mem, &["alpha"], "", 2, None, None, None)
            .await
            .unwrap();
        let fallback = Memory::recall_for_agents(&mem, &["alpha"], "keyword", 2, None, None, None)
            .await
            .unwrap();
        server.join().unwrap();

        assert_eq!(recent.len(), 2);
        assert_eq!(fallback.len(), 2);
    }

    #[test]
    fn refresh_embedder_swaps_embedder_in_place() {
        let mem = QdrantMemory::new_lazy(
            "test",
            "http://localhost:6333",
            "mem",
            None,
            Arc::new(super::super::embeddings::NoopEmbedding), // dims 0
        );
        assert_eq!(mem.embedder_dimensions(), 0);

        Memory::refresh_embedder(
            &mem,
            "openai",
            Some("sk-test"),
            "text-embedding-3-small",
            1536,
        );

        assert_eq!(
            mem.embedder_dimensions(),
            1536,
            "refresh_embedder must install the resolved provider's embedder"
        );
    }

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
            importance: None,
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
            importance: None,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("session_id"));
        assert!(!json.contains("agent_id"));
    }

    struct FixedEmbedding;

    #[async_trait::async_trait]
    impl EmbeddingProvider for FixedEmbedding {
        fn name(&self) -> &str {
            "fixed"
        }

        fn dimensions(&self) -> usize {
            3
        }

        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
        }
    }

    fn scored(id: &str, score: f64) -> QdrantScoredPoint {
        QdrantScoredPoint {
            id: serde_json::json!(id),
            score,
            payload: Some(MemoryPayload {
                key: id.to_string(),
                content: format!("content-{id}"),
                category: "core".into(),
                timestamp: "2026-11-02T00:00:00Z".into(),
                session_id: None,
                agent_id: Some("alpha".into()),
                importance: None,
            }),
        }
    }

    #[test]
    fn merge_ranked_blends_dense_and_bm25_like_sqlite() {
        // Dense cosine is already 0-1; BM25 is normalized by its max, then the
        // two are weighted 0.7 / 0.3 over the weights each point has.
        let dense = vec![scored("a", 0.9), scored("b", 0.5)];
        let keyword = vec![scored("b", 8.0), scored("c", 4.0)];
        let merged = QdrantMemory::merge_ranked(dense, keyword, 0.7, 0.3, 10);

        let got: Vec<(String, f64)> = merged
            .iter()
            .map(|p| (p.id.as_str().unwrap().to_string(), p.score))
            .collect();
        let score = |id: &str| got.iter().find(|(i, _)| i == id).unwrap().1;

        assert_eq!(got.len(), 3, "union of both legs: {got:?}");
        assert!((score("a") - 0.9).abs() < 1e-6, "dense-only keeps cosine");
        assert!((score("b") - (0.7 * 0.5 + 0.3 * 1.0)).abs() < 1e-6);
        assert!((score("c") - 0.5).abs() < 1e-6, "keyword-only: 4/8");
        assert_eq!(got[0].0, "a", "highest blended score first: {got:?}");
        assert!(
            merged.iter().all(|p| p.payload.is_some()),
            "payloads survive the merge"
        );
    }

    #[test]
    fn payload_importance_round_trips_and_defaults_to_none() {
        let legacy: MemoryPayload = serde_json::from_value(serde_json::json!({
            "key": "k", "content": "c", "category": "core",
            "timestamp": "2026-11-02T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(legacy.importance, None, "points written before importance");

        let mut payload = legacy;
        payload.importance = Some(0.8);
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["importance"], 0.8);
    }

    #[tokio::test]
    async fn hybrid_collection_stores_bm25_and_recalls_with_both_legs() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::sync::Mutex as StdMutex;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<StdMutex<Vec<(String, serde_json::Value)>>> = Arc::default();
        let seen_server = Arc::clone(&seen);
        let server = thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let request = read_http_request_any(&mut stream);
                let line = request.lines().next().unwrap().to_string();
                let body: serde_json::Value = request
                    .split_once("\r\n\r\n")
                    .and_then(|(_, b)| serde_json::from_str(b).ok())
                    .unwrap_or_default();
                let path = line.split_whitespace().nth(1).unwrap().to_string();
                let response = if line.starts_with("GET /collections/mem ") {
                    serde_json::json!({"result": {"config": {"params": {
                        "vectors": {"size": 3, "distance": "Cosine"},
                        "sparse_vectors": {"bm25": {"modifier": "idf"}}
                    }}, "points_count": 0}})
                } else if path.contains("/points/scroll") {
                    serde_json::json!({"result": {"points": [], "next_page_offset": null}})
                } else if path.contains("/points/search") {
                    serde_json::json!({"result": [
                        {"id": "p1", "score": 0.9, "payload": {"key": "dense_hit",
                          "content": "c1", "category": "core",
                          "timestamp": "2026-11-02T00:00:00Z", "agent_id": "alpha",
                          "importance": 0.8}}
                    ]})
                } else if path.contains("/points/query") {
                    serde_json::json!({"result": {"points": [
                        {"id": "p2", "score": 3.0, "payload": {"key": "air_filter",
                          "content": "c2", "category": "core",
                          "timestamp": "2026-11-02T00:00:00Z", "agent_id": "alpha"}}
                    ]}})
                } else {
                    serde_json::json!({"result": {"status": "completed"}, "status": "ok"})
                };
                let done = path.contains("/points/query");
                seen_server.lock().unwrap().push((line, body));
                let response = response.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .unwrap();
                if done {
                    break;
                }
            }
        });

        let mem = QdrantMemory::new_lazy("test", &endpoint, "mem", None, Arc::new(FixedEmbedding))
            .with_hybrid(true);
        mem.store_with_agent(
            "air_filter",
            "changed the air filter",
            MemoryCategory::Core,
            None,
            None,
            Some(0.8),
            Some("alpha"),
        )
        .await
        .unwrap();
        let recalled = mem
            .recall_for_agents(&["alpha"], "air filter", 5, None, None, None)
            .await
            .unwrap();
        server.join().unwrap();

        let seen = seen.lock().unwrap();
        let upsert = &seen
            .iter()
            .find(|(line, _)| line.starts_with("PUT /collections/mem/points"))
            .expect("store must upsert")
            .1;
        let point = &upsert["points"][0];
        assert_eq!(point["vector"][""], serde_json::json!([1.0, 0.0, 0.0]));
        assert_eq!(point["vector"]["bm25"]["model"], "qdrant/bm25");
        assert_eq!(
            point["vector"]["bm25"]["text"],
            "air_filter\nchanged the air filter"
        );
        assert_eq!(point["payload"]["importance"], 0.8);

        let query = &seen
            .iter()
            .find(|(line, _)| line.contains("/points/query"))
            .expect("hybrid recall must run the BM25 leg")
            .1;
        assert_eq!(query["using"], "bm25");
        assert_eq!(query["query"]["text"], "air filter");
        assert!(
            query["filter"]["must"]
                .as_array()
                .is_some_and(|m| m.iter().any(|c| c["key"] == "agent_id")),
            "the BM25 leg keeps the agent scope"
        );

        let keys: Vec<&str> = recalled.iter().map(|e| e.key.as_str()).collect();
        // A keyword-only hit with the top BM25 score blends to 1.0 (weights are
        // normalized over the legs a point appears in, as in SQLite), so it
        // outranks the 0.9 dense-only hit.
        assert_eq!(keys, vec!["air_filter", "dense_hit"]);
        assert_eq!(recalled[1].importance, Some(0.8));
        assert!(recalled.iter().all(|e| e.score.is_some_and(|s| s <= 1.0)));
    }

    /// Like `read_http_request`, but also accepts body-less requests (GET).
    fn read_http_request_any(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;

        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "Qdrant fixture closed mid-request");
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let header = String::from_utf8_lossy(&request[..header_end]);
            let content_length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                })
                .map_or(0, |v| v.parse::<usize>().unwrap());
            if request.len() >= header_end + 4 + content_length {
                return String::from_utf8(request).unwrap();
            }
        }
    }
}
