//! Multi-stage retrieval pipeline.
//!
//! Wraps a `Memory` trait object with staged retrieval:
//! - **Stage 1 (Hot cache):** In-memory LRU of recent recall results.
//! - **Stage 2 (FTS):** FTS5 keyword search with optional early-return.
//! - **Stage 3 (Vector):** Vector similarity search + hybrid merge.
//! - **Stage 4 (Rerank):** Optional cross-encoder reranking via external server.
//!
//! Configurable via `[memory]` settings: `retrieval_stages`, `fts_early_return_score`,
//! `rerank_enabled`, `rerank_threshold`, `rerank_url`.

use super::traits::{Memory, MemoryEntry};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A cached recall result.
struct CachedResult {
    entries: Vec<MemoryEntry>,
    created_at: Instant,
}

/// Multi-stage retrieval pipeline configuration.
#[derive(Debug, Clone)]
pub struct RetrievalConfig {
    /// Ordered list of stages: "cache", "fts", "vector".
    pub stages: Vec<String>,
    /// FTS score above which to early-return without vector stage.
    pub fts_early_return_score: f64,
    /// Max entries in the hot cache.
    pub cache_max_entries: usize,
    /// TTL for cached results.
    pub cache_ttl: Duration,
    /// Enable cross-encoder reranking.
    pub rerank_enabled: bool,
    /// Minimum candidate count to trigger reranking.
    pub rerank_threshold: usize,
    /// Reranker server URL (e.g. "http://localhost:8787").
    pub rerank_url: Option<String>,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            stages: vec!["cache".into(), "fts".into(), "vector".into()],
            fts_early_return_score: 0.85,
            cache_max_entries: 256,
            cache_ttl: Duration::from_secs(300),
            rerank_enabled: false,
            rerank_threshold: 5,
            rerank_url: None,
        }
    }
}

/// Multi-stage retrieval pipeline wrapping a `Memory` backend.
pub struct RetrievalPipeline {
    memory: Arc<dyn Memory>,
    config: RetrievalConfig,
    hot_cache: Mutex<HashMap<String, CachedResult>>,
}

impl RetrievalPipeline {
    pub fn new(memory: Arc<dyn Memory>, config: RetrievalConfig) -> Self {
        Self {
            memory,
            config,
            hot_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Build a cache key from query parameters.
    fn cache_key(
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        namespace: Option<&str>,
    ) -> String {
        format!(
            "{}:{}:{}:{}",
            query,
            limit,
            session_id.unwrap_or(""),
            namespace.unwrap_or("")
        )
    }

    /// Check the hot cache for a previous result.
    fn check_cache(&self, key: &str) -> Option<Vec<MemoryEntry>> {
        let cache = self.hot_cache.lock();
        if let Some(cached) = cache.get(key) {
            if cached.created_at.elapsed() < self.config.cache_ttl {
                return Some(cached.entries.clone());
            }
        }
        None
    }

    /// Store a result in the hot cache with LRU eviction.
    fn store_in_cache(&self, key: String, entries: Vec<MemoryEntry>) {
        let mut cache = self.hot_cache.lock();

        // LRU eviction: remove oldest entries if at capacity
        if cache.len() >= self.config.cache_max_entries {
            let oldest_key = cache
                .iter()
                .min_by_key(|(_, v)| v.created_at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest_key {
                cache.remove(&k);
            }
        }

        cache.insert(
            key,
            CachedResult {
                entries,
                created_at: Instant::now(),
            },
        );
    }

    /// Call external reranker server to reorder results by relevance.
    ///
    /// Falls back to original order on any error.
    async fn rerank_results(
        &self,
        query: &str,
        results: Vec<MemoryEntry>,
    ) -> Vec<MemoryEntry> {
        let url = match &self.config.rerank_url {
            Some(u) if !u.is_empty() => u.clone(),
            _ => return results,
        };

        if results.len() < self.config.rerank_threshold {
            return results;
        }

        let documents: Vec<String> = results
            .iter()
            .map(|e| format!("{}: {}", e.key, e.content))
            .collect();

        let body = serde_json::json!({
            "query": query,
            "documents": documents,
        });

        let client = crate::config::build_runtime_proxy_client("memory.reranker");
        let rerank_endpoint = format!("{}/rerank", url.trim_end_matches('/'));

        let resp = match client
            .post(&rerank_endpoint)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("reranker request failed: {e}");
                return results;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!("reranker returned {status}: {text}");
            return results;
        }

        #[derive(serde::Deserialize)]
        struct RerankResult {
            index: usize,
            score: f64,
        }

        #[derive(serde::Deserialize)]
        struct RerankResponse {
            results: Vec<RerankResult>,
        }

        let reranked: RerankResponse = match resp.json().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("reranker response parse failed: {e}");
                return results;
            }
        };

        tracing::debug!(
            "reranker returned {} scored results",
            reranked.results.len()
        );

        // Reorder entries by reranker scores
        let mut reordered: Vec<MemoryEntry> = Vec::with_capacity(reranked.results.len());
        for rr in &reranked.results {
            if rr.index < results.len() {
                let mut entry = results[rr.index].clone();
                entry.score = Some(rr.score);
                reordered.push(entry);
            }
        }

        // Append any entries the reranker didn't return (shouldn't happen,
        // but be defensive)
        let reranked_indices: std::collections::HashSet<usize> =
            reranked.results.iter().map(|r| r.index).collect();
        for (i, entry) in results.into_iter().enumerate() {
            if !reranked_indices.contains(&i) {
                reordered.push(entry);
            }
        }

        reordered
    }

    /// Execute the multi-stage retrieval pipeline.
    pub async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        namespace: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let ck = Self::cache_key(query, limit, session_id, namespace);

        for stage in &self.config.stages {
            match stage.as_str() {
                "cache" => {
                    if let Some(cached) = self.check_cache(&ck) {
                        tracing::debug!("retrieval pipeline: cache hit for '{query}'");
                        return Ok(cached);
                    }
                }
                "fts" | "vector" => {
                    // Both FTS and vector are handled by the backend's recall method
                    // which already does hybrid merge. We delegate to it.
                    let results = if let Some(ns) = namespace {
                        self.memory
                            .recall_namespaced(ns, query, limit, session_id, since, until)
                            .await?
                    } else {
                        self.memory
                            .recall(query, limit, session_id, since, until)
                            .await?
                    };

                    if !results.is_empty() {
                        // Check for FTS early-return: if top score exceeds threshold
                        // and we're in the FTS stage, we can skip further stages
                        if stage == "fts" {
                            if let Some(top_score) = results.first().and_then(|e| e.score) {
                                if top_score >= self.config.fts_early_return_score {
                                    tracing::debug!(
                                        "retrieval pipeline: FTS early return (score={top_score:.3})"
                                    );
                                    self.store_in_cache(ck, results.clone());
                                    return Ok(results);
                                }
                            }
                        }

                        // Apply reranking if enabled
                        let results = if self.config.rerank_enabled {
                            self.rerank_results(query, results).await
                        } else {
                            results
                        };

                        self.store_in_cache(ck, results.clone());
                        return Ok(results);
                    }
                }
                other => {
                    tracing::warn!("retrieval pipeline: unknown stage '{other}', skipping");
                }
            }
        }

        // No results from any stage
        Ok(Vec::new())
    }

    /// Invalidate the hot cache (e.g. after a store operation).
    pub fn invalidate_cache(&self) {
        self.hot_cache.lock().clear();
    }

    /// Get the number of entries in the hot cache.
    pub fn cache_size(&self) -> usize {
        self.hot_cache.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::NoneMemory;

    #[tokio::test]
    async fn pipeline_returns_empty_from_none_backend() {
        let memory = Arc::new(NoneMemory::new());
        let pipeline = RetrievalPipeline::new(memory, RetrievalConfig::default());

        let results = pipeline
            .recall("test", 10, None, None, None, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn pipeline_cache_invalidation() {
        let memory = Arc::new(NoneMemory::new());
        let pipeline = RetrievalPipeline::new(memory, RetrievalConfig::default());

        // Force a cache entry
        let ck = RetrievalPipeline::cache_key("test", 10, None, None);
        pipeline.store_in_cache(ck, vec![]);

        assert_eq!(pipeline.cache_size(), 1);
        pipeline.invalidate_cache();
        assert_eq!(pipeline.cache_size(), 0);
    }

    #[test]
    fn cache_key_includes_all_params() {
        let k1 = RetrievalPipeline::cache_key("hello", 10, Some("sess-a"), Some("ns1"));
        let k2 = RetrievalPipeline::cache_key("hello", 10, Some("sess-b"), Some("ns1"));
        let k3 = RetrievalPipeline::cache_key("hello", 10, Some("sess-a"), Some("ns2"));

        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[tokio::test]
    async fn pipeline_caches_results() {
        let memory = Arc::new(NoneMemory::new());
        let config = RetrievalConfig {
            stages: vec!["cache".into()],
            ..Default::default()
        };
        let pipeline = RetrievalPipeline::new(memory, config);

        // First call: cache miss, no results
        let results = pipeline
            .recall("test", 10, None, None, None, None)
            .await
            .unwrap();
        assert!(results.is_empty());

        // Manually insert a cache entry
        let ck = RetrievalPipeline::cache_key("cached_query", 5, None, None);
        let fake_entry = MemoryEntry {
            id: "1".into(),
            key: "k".into(),
            content: "cached content".into(),
            category: crate::memory::MemoryCategory::Core,
            timestamp: "now".into(),
            session_id: None,
            score: Some(0.9),
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
        };
        pipeline.store_in_cache(ck, vec![fake_entry]);

        // Cache hit
        let results = pipeline
            .recall("cached_query", 5, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "cached content");
    }

    #[test]
    fn rerank_config_defaults() {
        let config = RetrievalConfig::default();
        assert!(!config.rerank_enabled);
        assert_eq!(config.rerank_threshold, 5);
        assert!(config.rerank_url.is_none());
    }
}
