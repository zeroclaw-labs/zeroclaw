//! Knowledge management tool for capturing, searching, and reusing expertise.
//!
//! Exposes the knowledge graph to the agent via the `Tool` trait with actions:
//! capture, search, relate, suggest, expert_find, lessons_extract, graph_stats.

use super::traits::{Tool, ToolResult};
use crate::memory::knowledge_graph::{KnowledgeGraph, NodeType, Relation};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool for managing a knowledge graph of patterns, decisions, lessons, and experts.
pub struct KnowledgeTool {
    graph: Arc<KnowledgeGraph>,
}

impl KnowledgeTool {
    pub fn new(graph: Arc<KnowledgeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for KnowledgeTool {
    fn name(&self) -> &str {
        "knowledge"
    }

    fn description(&self) -> &str {
        "Manage a knowledge graph of architecture decisions, solution patterns, lessons learned, and experts. Actions: capture, search, relate, suggest, expert_find, lessons_extract, graph_stats."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["capture", "search", "relate", "suggest", "expert_find", "lessons_extract", "graph_stats"],
                    "description": "The action to perform"
                },
                "node_type": {
                    "type": "string",
                    "enum": ["pattern", "decision", "lesson", "expert", "technology"],
                    "description": "Type of knowledge node (for capture)"
                },
                "title": {
                    "type": "string",
                    "description": "Title for the knowledge item (for capture)"
                },
                "content": {
                    "type": "string",
                    "description": "Content body (for capture) or text to extract lessons from (for lessons_extract)"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tags for filtering and categorization"
                },
                "source_project": {
                    "type": "string",
                    "description": "Source project identifier (for capture)"
                },
                "query": {
                    "type": "string",
                    "description": "Search query text (for search, suggest)"
                },
                "from_id": {
                    "type": "string",
                    "description": "Source node ID (for relate)"
                },
                "to_id": {
                    "type": "string",
                    "description": "Target node ID (for relate)"
                },
                "relation": {
                    "type": "string",
                    "enum": ["uses", "replaces", "extends", "authored_by", "applies_to"],
                    "description": "Relationship type (for relate)"
                },
                "filters": {
                    "type": "object",
                    "properties": {
                        "node_type": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "project": { "type": "string" }
                    },
                    "description": "Optional search filters"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'action' parameter"))?;

        match action {
            "capture" => self.handle_capture(&args),
            "search" => self.handle_search(&args),
            "relate" => self.handle_relate(&args),
            "suggest" => self.handle_suggest(&args),
            "expert_find" => self.handle_expert_find(&args),
            "lessons_extract" => self.handle_lessons_extract(&args),
            "graph_stats" => self.handle_graph_stats(),
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("unknown action: {other}")),
            }),
        }
    }
}

impl KnowledgeTool {
    fn handle_capture(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let node_type_str = args
            .get("node_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'node_type' for capture"))?;
        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'title' for capture"))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'content' for capture"))?;

        let node_type = NodeType::parse(node_type_str).map_err(|e| anyhow::anyhow!("{e}"))?;

        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let source_project = args.get("source_project").and_then(|v| v.as_str());

        match self
            .graph
            .add_node(node_type, title, content, &tags, source_project)
        {
            Ok(id) => Ok(ToolResult {
                success: true,
                output: json!({ "node_id": id }).to_string(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("capture failed: {e}")),
            }),
        }
    }

    fn handle_search(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

        // Apply optional filters.
        let filter_tags: Vec<String> = args
            .get("filters")
            .and_then(|f| f.get("tags"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let filter_type = args
            .get("filters")
            .and_then(|f| f.get("node_type"))
            .and_then(|v| v.as_str());

        let results = if query.is_empty() && !filter_tags.is_empty() {
            // Tag-only search.
            let nodes = self.graph.query_by_tags(&filter_tags)?;
            nodes
                .into_iter()
                .map(|node| json!({ "id": node.id, "type": node.node_type, "title": node.title, "score": 1.0 }))
                .collect::<Vec<_>>()
        } else if !query.is_empty() {
            let mut search_results = self.graph.query_by_similarity(query, 20)?;

            // Post-filter by type if specified.
            if let Some(ft) = filter_type {
                if let Ok(nt) = NodeType::parse(ft) {
                    search_results.retain(|r| r.node.node_type == nt);
                }
            }
            // Post-filter by tags if specified.
            if !filter_tags.is_empty() {
                search_results.retain(|r| filter_tags.iter().all(|t| r.node.tags.contains(t)));
            }

            search_results
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.node.id,
                        "type": r.node.node_type,
                        "title": r.node.title,
                        "score": r.score
                    })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        Ok(ToolResult {
            success: true,
            output: json!({ "results": results, "count": results.len() }).to_string(),
            error: None,
        })
    }

    fn handle_relate(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let from_id = args
            .get("from_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'from_id' for relate"))?;
        let to_id = args
            .get("to_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'to_id' for relate"))?;
        let relation_str = args
            .get("relation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'relation' for relate"))?;

        let relation = Relation::parse(relation_str).map_err(|e| anyhow::anyhow!("{e}"))?;

        match self.graph.add_edge(from_id, to_id, relation) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: "relationship created".to_string(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("relate failed: {e}")),
            }),
        }
    }

    fn handle_suggest(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .or_else(|| args.get("content"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'query' or 'content' for suggest"))?;

        let results = self.graph.query_by_similarity(query, 10)?;
        let suggestions: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                json!({
                    "id": r.node.id,
                    "type": r.node.node_type,
                    "title": r.node.title,
                    "content_preview": truncate_str(&r.node.content, 200),
                    "tags": r.node.tags,
                    "relevance_score": r.score,
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: json!({ "suggestions": suggestions, "count": suggestions.len() }).to_string(),
            error: None,
        })
    }

    fn handle_expert_find(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        if tags.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("missing 'tags' for expert_find".into()),
            });
        }

        let experts = self.graph.find_experts(&tags)?;
        let output: Vec<serde_json::Value> = experts
            .into_iter()
            .map(|r| {
                json!({
                    "id": r.node.id,
                    "name": r.node.title,
                    "contribution_score": r.score,
                    "tags": r.node.tags,
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: json!({ "experts": output, "count": output.len() }).to_string(),
            error: None,
        })
    }

    fn handle_lessons_extract(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let text = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'content' for lessons_extract"))?;

        // Simple keyword-based extraction: split on sentence boundaries, score by
        // signal keywords that commonly indicate lessons.
        let signal_words = [
            "learned",
            "lesson",
            "mistake",
            "should have",
            "next time",
            "improvement",
            "better",
            "avoid",
            "risk",
            "issue",
            "root cause",
            "takeaway",
            "insight",
            "recommendation",
            "decision",
        ];

        let sentences: Vec<&str> = text
            .split(&['.', '!', '?', '\n'][..])
            .map(str::trim)
            .filter(|s| s.len() > 10)
            .collect();

        let mut lessons: Vec<serde_json::Value> = Vec::new();
        for sentence in &sentences {
            let lower = sentence.to_ascii_lowercase();
            let score: f64 = signal_words.iter().filter(|w| lower.contains(**w)).count() as f64;
            if score > 0.0 {
                lessons.push(json!({
                    "text": sentence,
                    "confidence": (score / signal_words.len() as f64).min(1.0),
                }));
            }
        }

        lessons.sort_by(|a, b| {
            let sa = a["confidence"].as_f64().unwrap_or(0.0);
            let sb = b["confidence"].as_f64().unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        lessons.truncate(10);

        Ok(ToolResult {
            success: true,
            output: json!({ "lessons": lessons, "count": lessons.len() }).to_string(),
            error: None,
        })
    }

    fn handle_graph_stats(&self) -> anyhow::Result<ToolResult> {
        match self.graph.stats() {
            Ok(stats) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string(&stats).unwrap_or_default(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("failed to get stats: {e}")),
            }),
        }
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::knowledge_graph::KnowledgeGraph;
    use tempfile::TempDir;

    fn test_tool() -> (TempDir, KnowledgeTool) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        let graph = Arc::new(KnowledgeGraph::new(&db_path, 10000).unwrap());
        (tmp, KnowledgeTool::new(graph))
    }

    #[tokio::test]
    async fn capture_returns_node_id() {
        let (_tmp, tool) = test_tool();
        let result = tool
            .execute(json!({
                "action": "capture",
                "node_type": "pattern",
                "title": "Circuit Breaker",
                "content": "Use circuit breaker for external calls",
                "tags": ["resilience", "microservices"]
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(output["node_id"].is_string());
    }

    #[tokio::test]
    async fn search_returns_results() {
        let (_tmp, tool) = test_tool();
        tool.execute(json!({
            "action": "capture",
            "node_type": "decision",
            "title": "Use Kubernetes",
            "content": "Kubernetes for container orchestration",
            "tags": ["infrastructure"]
        }))
        .await
        .unwrap();

        let result = tool
            .execute(json!({
                "action": "search",
                "query": "Kubernetes container"
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(output["count"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn relate_creates_edge() {
        let (_tmp, tool) = test_tool();

        let r1 = tool
            .execute(json!({
                "action": "capture",
                "node_type": "pattern",
                "title": "CQRS",
                "content": "Command Query Responsibility Segregation"
            }))
            .await
            .unwrap();
        let id1: serde_json::Value = serde_json::from_str(&r1.output).unwrap();

        let r2 = tool
            .execute(json!({
                "action": "capture",
                "node_type": "technology",
                "title": "Event Sourcing",
                "content": "Event sourcing pattern"
            }))
            .await
            .unwrap();
        let id2: serde_json::Value = serde_json::from_str(&r2.output).unwrap();

        let result = tool
            .execute(json!({
                "action": "relate",
                "from_id": id1["node_id"],
                "to_id": id2["node_id"],
                "relation": "uses"
            }))
            .await
            .unwrap();

        assert!(result.success);
    }

    #[tokio::test]
    async fn graph_stats_reports_counts() {
        let (_tmp, tool) = test_tool();
        tool.execute(json!({
            "action": "capture",
            "node_type": "lesson",
            "title": "Test lesson",
            "content": "Testing matters"
        }))
        .await
        .unwrap();

        let result = tool
            .execute(json!({ "action": "graph_stats" }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["total_nodes"].as_u64().unwrap(), 1);
    }

    #[tokio::test]
    async fn lessons_extract_finds_signal_sentences() {
        let (_tmp, tool) = test_tool();
        let result = tool
            .execute(json!({
                "action": "lessons_extract",
                "content": "The project went well overall. We learned that caching is critical. Next time we should avoid tight coupling. The weather was nice."
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(output["count"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let (_tmp, tool) = test_tool();
        let result = tool
            .execute(json!({ "action": "delete_all" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("unknown action"));
    }

    #[test]
    fn name_and_schema_are_valid() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        let graph = Arc::new(KnowledgeGraph::new(&db_path, 100).unwrap());
        let tool = KnowledgeTool::new(graph);

        assert_eq!(tool.name(), "knowledge");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
    }
}
