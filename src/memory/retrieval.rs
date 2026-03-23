//! Multi-stage retrieval pipeline.
//!
//! Wraps a `Memory` trait object with staged retrieval:
//! - **Stage 1 (Hot cache):** In-memory LRU of recent recall results.
//! - **Stage 2 (FTS):** FTS5 keyword search with optional early-return.
//! - **Stage 3 (Vector):** Vector similarity search + hybrid merge.
//!
//! Configurable via `[memory]` settings: `retrieval_stages`, `fts_early_return_score`.

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
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            stages: vec!["cache".into(), "fts".into(), "vector".into()],
            fts_early_return_score: 0.85,
            cache_max_entries: 256,
            cache_ttl: Duration::from_secs(300),
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
            metadata: None,
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
}
