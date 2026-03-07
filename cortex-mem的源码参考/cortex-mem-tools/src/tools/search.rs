// Search Tools - Vector-based semantic search

use crate::{MemoryOperations, Result, types::*};
use cortex_mem_core::{ContextLayer, FilesystemOperations, SearchOptions};

impl MemoryOperations {
    /// Semantic search using vector similarity
    ///
    /// Uses directory recursive retrieval strategy:
    /// 1. Intent Analysis - Analyze query intent
    /// 2. Initial Positioning - Locate high-score directories via L0
    /// 3. Refined Exploration - Search within directories
    /// 4. Recursive Drill-down - Explore subdirectories
    /// 5. Result Aggregation - Sort and deduplicate
    pub async fn search(&self, args: SearchArgs) -> Result<SearchResponse> {
        // Normalize scope before searching
        let normalized_args = SearchArgs {
            scope: args
                .scope
                .as_deref()
                .map(|s| Self::normalize_scope(Some(s))),
            ..args
        };

        // Use vector search engine
        let raw_results = self.vector_search(&normalized_args).await?;

        // Enrich results with requested layers
        let enriched_results = self
            .enrich_results(
                raw_results,
                &normalized_args
                    .return_layers
                    .clone()
                    .unwrap_or(vec!["L0".to_string()]),
            )
            .await?;

        let total = enriched_results.len();

        Ok(SearchResponse {
            query: normalized_args.query.clone(),
            results: enriched_results,
            total,
            engine_used: "vector".to_string(),
        })
    }

    /// Simple find - quick search returning only L0 abstracts
    pub async fn find(&self, args: FindArgs) -> Result<FindResponse> {
        let normalized_scope = Self::normalize_scope(args.scope.as_deref());

        let search_args = SearchArgs {
            query: args.query.clone(),
            recursive: Some(true),
            return_layers: Some(vec!["L0".to_string()]),
            scope: Some(normalized_scope),
            limit: args.limit,
        };

        let search_response = self.search(search_args).await?;

        let results = search_response
            .results
            .into_iter()
            .map(|r| FindResult {
                uri: r.uri,
                abstract_text: r.abstract_text.unwrap_or_default(),
            })
            .collect();

        Ok(FindResponse {
            query: args.query,
            results,
            total: search_response.total,
        })
    }

    /// Normalize scope parameter to ensure it's a valid cortex URI
    fn normalize_scope(scope: Option<&str>) -> String {
        match scope {
            None => "cortex://session".to_string(),
            Some(s) => {
                if s.starts_with("cortex://") {
                    let dimension = s
                        .strip_prefix("cortex://")
                        .and_then(|rest| rest.split('/').next())
                        .unwrap_or("");

                    match dimension {
                        "resources" | "user" | "agent" | "session" => s.to_string(),
                        // Legacy aliases - map to new structure
                        "threads" | "agents" | "users" | "global" => {
                            let rest = s
                                .strip_prefix("cortex://")
                                .and_then(|r| r.find('/').map(|pos| &r[pos..]))
                                .unwrap_or("");
                            format!("cortex://session{}", rest)
                        }
                        "system" | "assistant" | "bot" => "cortex://session".to_string(),
                        _ => "cortex://session".to_string(),
                    }
                } else {
                    format!("cortex://session/{}", s.trim_start_matches('/'))
                }
            }
        }
    }

    // ==================== Internal Methods ====================

    /// Vector search using VectorSearchEngine
    /// Uses layered semantic search (L0->L1->L2) for optimal retrieval
    async fn vector_search(&self, args: &SearchArgs) -> Result<Vec<RawSearchResult>> {
        let search_options = SearchOptions {
            limit: args.limit.unwrap_or(10),
            threshold: 0.5,
            root_uri: args.scope.clone(),
            recursive: args.recursive.unwrap_or(true),
        };

        // Use layered semantic search for L0/L1/L2 tiered retrieval
        let results = self
            .vector_engine
            .layered_semantic_search(&args.query, &search_options)
            .await?;

        Ok(results
            .into_iter()
            .map(|r| RawSearchResult {
                uri: r.uri,
                score: r.score,
            })
            .collect())
    }

    /// Enrich raw results with requested layers
    async fn enrich_results(
        &self,
        raw_results: Vec<RawSearchResult>,
        return_layers: &[String],
    ) -> Result<Vec<SearchResult>> {
        let mut enriched = Vec::new();

        for raw in raw_results {
            let mut result = SearchResult {
                uri: raw.uri.clone(),
                score: raw.score,
                abstract_text: None,
                overview_text: None,
                content: None,
            };

            // Load layers as requested
            if return_layers.contains(&"L0".to_string()) {
                result.abstract_text = self
                    .layer_manager
                    .load(&raw.uri, ContextLayer::L0Abstract)
                    .await
                    .ok();
            }
            if return_layers.contains(&"L1".to_string()) {
                result.overview_text = self
                    .layer_manager
                    .load(&raw.uri, ContextLayer::L1Overview)
                    .await
                    .ok();
            }
            if return_layers.contains(&"L2".to_string()) {
                result.content = self.filesystem.read(&raw.uri).await.ok();
            }

            enriched.push(result);
        }

        Ok(enriched)
    }
}
