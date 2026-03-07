// Filesystem Tools

use crate::{MemoryOperations, Result, types::*};
use cortex_mem_core::{ContextLayer, FilesystemOperations};

impl MemoryOperations {
    /// List directory contents
    pub async fn ls(&self, args: LsArgs) -> Result<LsResponse> {
        // Use default URI if empty
        let uri = if args.uri.is_empty() {
            "cortex://session".to_string()
        } else {
            args.uri
        };
        let entries = self.filesystem.list(&uri).await?;

        let mut result_entries = Vec::new();
        for entry in entries {
            let child_count = if entry.is_directory {
                Some(self.filesystem.list(&entry.uri).await?.len())
            } else {
                None
            };

            let mut result_entry = LsEntry {
                name: entry.name.clone(),
                uri: entry.uri.clone(),
                is_directory: entry.is_directory,
                child_count,
                abstract_text: None,
            };

            // Include abstracts if requested and entry is a file
            if args.include_abstracts.unwrap_or(false) && !entry.is_directory {
                result_entry.abstract_text = self
                    .layer_manager
                    .load(&entry.uri, ContextLayer::L0Abstract)
                    .await
                    .ok();
            }

            result_entries.push(result_entry);
        }

        Ok(LsResponse {
            uri,
            total: result_entries.len(),
            entries: result_entries,
        })
    }

    /// Explore memory space intelligently
    pub async fn explore(&self, args: ExploreArgs) -> Result<ExploreResponse> {
        let start_uri = args.start_uri.unwrap_or("cortex://session".to_string());
        let max_depth = args.max_depth.unwrap_or(3);
        let return_layers = args.return_layers.unwrap_or(vec!["L0".to_string()]);

        let mut exploration_path = Vec::new();
        let mut all_matches = Vec::new();
        let mut total_explored = 0;

        // Start exploration
        self.explore_recursive(
            &args.query,
            &start_uri,
            0,
            max_depth,
            &return_layers,
            &mut exploration_path,
            &mut all_matches,
            &mut total_explored,
        )
        .await?;

        Ok(ExploreResponse {
            query: args.query,
            exploration_path,
            total_explored,
            total_matches: all_matches.len(),
            matches: all_matches,
        })
    }

    // ==================== Internal Methods ====================

    #[allow(clippy::too_many_arguments)]
    fn explore_recursive<'a>(
        &'a self,
        query: &'a str,
        uri: &'a str,
        depth: usize,
        max_depth: usize,
        return_layers: &'a [String],
        path: &'a mut Vec<ExplorationPathItem>,
        matches: &'a mut Vec<SearchResult>,
        total_explored: &'a mut usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if depth > max_depth {
                return Ok(());
            }

            *total_explored += 1;

            // Get abstract for current URI
            let abstract_text = self
                .layer_manager
                .load(uri, ContextLayer::L0Abstract)
                .await
                .ok();

            // Check relevance (simple keyword matching for now)
            let relevance_score = if let Some(abs) = &abstract_text {
                if abs.to_lowercase().contains(&query.to_lowercase()) {
                    0.8
                } else {
                    0.4
                }
            } else {
                0.0
            };

            // Add to exploration path if relevant
            if relevance_score > 0.5 {
                path.push(ExplorationPathItem {
                    uri: uri.to_string(),
                    relevance_score,
                    abstract_text: abstract_text.clone(),
                });
            }

            // List entries
            let entries = self.filesystem.list(uri).await?;

            for entry in entries {
                if entry.is_directory {
                    // Recursively explore subdirectories
                    self.explore_recursive(
                        query,
                        &entry.uri,
                        depth + 1,
                        max_depth,
                        return_layers,
                        path,
                        matches,
                        total_explored,
                    )
                    .await?;
                } else {
                    // Check if file matches
                    if let Ok(content) = self.filesystem.read(&entry.uri).await {
                        if content.to_lowercase().contains(&query.to_lowercase()) {
                            // Enrich with requested layers
                            let mut result = SearchResult {
                                uri: entry.uri.clone(),
                                score: relevance_score,
                                abstract_text: None,
                                overview_text: None,
                                content: None,
                            };

                            if return_layers.contains(&"L0".to_string()) {
                                result.abstract_text = self
                                    .layer_manager
                                    .load(&entry.uri, ContextLayer::L0Abstract)
                                    .await
                                    .ok();
                            }
                            if return_layers.contains(&"L1".to_string()) {
                                result.overview_text = self
                                    .layer_manager
                                    .load(&entry.uri, ContextLayer::L1Overview)
                                    .await
                                    .ok();
                            }
                            if return_layers.contains(&"L2".to_string()) {
                                result.content = Some(content);
                            }

                            matches.push(result);
                        }
                    }
                }
            }

            Ok(())
        })
    }
}
