// Multi-Query Expansion (v3.0 S3)
//
// Before Phase 1 search, expand a user query into 3–5 semantic variations
// using a fast SLM (Haiku). Each variation is searched independently and
// results are fused via RRF, improving recall for ambiguous or domain-specific
// queries (e.g. "이혼 소송" → {"이혼 절차", "협의이혼", "재산분할", "위자료"}).
//
// Cache: identical queries within 24h reuse the previous expansion.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::providers::traits::Provider;

/// Cache TTL for expanded queries (24 hours).
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Maximum cached entries before LRU-style eviction.
const CACHE_MAX: usize = 500;

/// Configuration for multi-query expansion.
#[derive(Debug, Clone)]
pub struct QueryExpandConfig {
    /// Enable multi-query expansion (default: false until validated).
    pub enabled: bool,
    /// Model to use for expansion (should be fast/cheap, e.g. Haiku).
    pub model: String,
    /// Number of query variations to generate (3–5).
    pub num_variations: usize,
}

impl Default for QueryExpandConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "claude-haiku-4-5-20251001".to_string(),
            num_variations: 4,
        }
    }
}

/// Query expander that generates semantic variations of a search query.
pub struct QueryExpander {
    cache: Mutex<HashMap<String, CachedExpansion>>,
}

struct CachedExpansion {
    queries: Vec<String>,
    created_at: Instant,
}

impl QueryExpander {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Expand a query into multiple semantic variations using an LLM.
    ///
    /// Returns the original query plus generated variations.
    /// If expansion fails, returns just the original query (graceful degradation).
    pub async fn expand(
        &self,
        query: &str,
        config: &QueryExpandConfig,
        provider: &dyn Provider,
    ) -> Vec<String> {
        if !config.enabled || query.trim().is_empty() {
            return vec![query.to_string()];
        }

        // Check cache
        if let Some(cached) = self.get_cached(query) {
            return cached;
        }

        // Call LLM for expansion
        match self.generate_variations(query, config, provider).await {
            Ok(variations) => {
                let mut result = vec![query.to_string()];
                result.extend(variations);
                self.put_cache(query, result.clone());
                result
            }
            Err(e) => {
                tracing::warn!("Query expansion failed, using original: {e}");
                vec![query.to_string()]
            }
        }
    }

    async fn generate_variations(
        &self,
        query: &str,
        config: &QueryExpandConfig,
        provider: &dyn Provider,
    ) -> Result<Vec<String>> {
        let system_prompt = format!(
            "You are a search query expansion assistant. \
             Given a user's search query, generate exactly {} alternative search queries \
             that capture different aspects, synonyms, or related concepts. \
             Output ONLY the queries, one per line, no numbering, no explanations. \
             The queries should be in the same language as the input.",
            config.num_variations
        );

        let response = provider
            .chat_with_system(
                Some(&system_prompt),
                query,
                &config.model,
                0.3, // low temperature for consistency
            )
            .await
            .context("LLM call for query expansion")?;

        let variations: Vec<String> = response
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && l.len() < 500)
            .take(config.num_variations)
            .collect();

        if variations.is_empty() {
            anyhow::bail!("LLM returned no valid query variations");
        }

        Ok(variations)
    }

    fn get_cached(&self, query: &str) -> Option<Vec<String>> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(query)?;
        if entry.created_at.elapsed() < CACHE_TTL {
            Some(entry.queries.clone())
        } else {
            None
        }
    }

    fn put_cache(&self, query: &str, queries: Vec<String>) {
        if let Ok(mut cache) = self.cache.lock() {
            // Simple eviction: clear half when full
            if cache.len() >= CACHE_MAX {
                let keys: Vec<String> = cache.keys().take(CACHE_MAX / 2).cloned().collect();
                for k in keys {
                    cache.remove(&k);
                }
            }
            cache.insert(
                query.to_string(),
                CachedExpansion {
                    queries,
                    created_at: Instant::now(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Stub provider that returns predetermined lines for query expansion.
    struct StubExpandProvider {
        response: String,
    }

    impl StubExpandProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
            }
        }
    }

    #[async_trait]
    impl Provider for StubExpandProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok(self.response.clone())
        }
    }

    #[test]
    fn default_config_disabled() {
        let cfg = QueryExpandConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.num_variations, 4);
    }

    #[tokio::test]
    async fn disabled_returns_original() {
        let expander = QueryExpander::new();
        let config = QueryExpandConfig::default(); // disabled
        let provider = StubExpandProvider::new("should not be called");
        let result = expander.expand("test query", &config, &provider).await;
        assert_eq!(result, vec!["test query"]);
    }

    #[tokio::test]
    async fn empty_query_returns_original() {
        let expander = QueryExpander::new();
        let config = QueryExpandConfig {
            enabled: true,
            ..Default::default()
        };
        let provider = StubExpandProvider::new("variation1");
        let result = expander.expand("", &config, &provider).await;
        assert_eq!(result, vec![""]);
    }

    #[tokio::test]
    async fn expands_query_with_llm() {
        let expander = QueryExpander::new();
        let config = QueryExpandConfig {
            enabled: true,
            num_variations: 3,
            ..Default::default()
        };
        let provider = StubExpandProvider::new("이혼 절차\n협의이혼\n재산분할");
        let result = expander
            .expand("이혼 소송", &config, &provider)
            .await;
        assert_eq!(result.len(), 4); // original + 3 variations
        assert_eq!(result[0], "이혼 소송");
        assert_eq!(result[1], "이혼 절차");
    }

    #[tokio::test]
    async fn caches_expansion_result() {
        let expander = QueryExpander::new();
        let config = QueryExpandConfig {
            enabled: true,
            num_variations: 2,
            ..Default::default()
        };
        let provider = StubExpandProvider::new("var1\nvar2");
        let first = expander.expand("cached query", &config, &provider).await;
        // Second call should hit cache (even with different provider response)
        let provider2 = StubExpandProvider::new("different\nresponse");
        let second = expander.expand("cached query", &config, &provider2).await;
        assert_eq!(first, second);
    }

    #[test]
    fn cache_stores_and_retrieves() {
        let expander = QueryExpander::new();
        let queries = vec!["q1".to_string(), "q2".to_string()];
        expander.put_cache("test", queries.clone());
        let cached = expander.get_cached("test");
        assert_eq!(cached, Some(queries));
    }

    #[test]
    fn cache_miss_on_unknown_key() {
        let expander = QueryExpander::new();
        assert!(expander.get_cached("unknown").is_none());
    }

    #[test]
    fn cache_eviction_when_full() {
        let expander = QueryExpander::new();
        for i in 0..CACHE_MAX + 10 {
            expander.put_cache(&format!("q{i}"), vec![format!("v{i}")]);
        }
        let cache = expander.cache.lock().unwrap();
        assert!(cache.len() <= CACHE_MAX);
    }
}
