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
        "Manage a knowledge graph of architecture decisions, solution patterns, lessons learned, experts, and client relationships. Actions: capture, search, relate, suggest, expert_find, lessons_extract, graph_stats, client_network, interaction_log."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["capture", "search", "relate", "suggest", "expert_find", "lessons_extract", "graph_stats", "client_network", "interaction_log"],
                    "description": "The action to perform"
                },
                "node_type": {
                    "type": "string",
                    "enum": ["pattern", "decision", "lesson", "expert", "technology", "client", "contact", "interaction"],
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
                    "enum": ["uses", "replaces", "extends", "authored_by", "applies_to", "manages_client", "contact_of", "interacted_with"],
                    "description": "Relationship type (for relate)"
                },
                "client_id": {
                    "type": "string",
                    "description": "Client node ID (for client_network, interaction_log)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results (for interaction_log)"
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
            "client_network" => self.handle_client_network(&args),
            "interaction_log" => self.handle_interaction_log(&args),
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

        let filter_project = args
            .get("filters")
            .and_then(|f| f.get("project"))
            .and_then(|v| v.as_str());

        // Parse the node_type filter once so it applies in all code paths.
        let parsed_filter_type = filter_type.and_then(|ft| NodeType::parse(ft).ok());

        let results = if query.is_empty() && !filter_tags.is_empty() {
            // Tag-only search -- apply node_type and project filters consistently.
            let mut nodes = self.graph.query_by_tags(&filter_tags)?;
            if let Some(ref nt) = parsed_filter_type {
                nodes.retain(|n| &n.node_type == nt);
            }
            if let Some(proj) = filter_project {
                nodes.retain(|n| n.source_project.as_deref() == Some(proj));
            }
            nodes
                .into_iter()
                .map(|node| json!({ "id": node.id, "type": node.node_type, "title": node.title, "score": 1.0 }))
                .collect::<Vec<_>>()
        } else if !query.is_empty() {
            let mut search_results = self.graph.query_by_similarity(query, 20)?;

            // Post-filter by type if specified.
            if let Some(ref nt) = parsed_filter_type {
                search_results.retain(|r| &r.node.node_type == nt);
            }
            // Post-filter by project if specified.
            if let Some(proj) = filter_project {
                search_results.retain(|r| r.node.source_project.as_deref() == Some(proj));
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

    fn handle_client_network(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let client_id = args
            .get("client_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'client_id' for client_network"))?;

        let client = self
            .graph
            .get_node(client_id)?
            .ok_or_else(|| anyhow::anyhow!("client node not found: {client_id}"))?;

        if client.node_type != NodeType::Client {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "node {} is not a client (is {})",
                    client_id,
                    client.node_type.as_str()
                )),
            });
        }

        // Get contacts and interactions pointing to this client
        let inbound = self.graph.find_inbound(client_id)?;
        let contacts: Vec<_> = inbound
            .iter()
            .filter(|(n, r)| n.node_type == NodeType::Contact && *r == Relation::ContactOf)
            .map(|(n, _)| json!({"id": &n.id, "name": &n.title, "tags": &n.tags}))
            .collect();

        let outbound = self.graph.find_related(client_id)?;
        let interactions: Vec<_> = outbound.iter()
            .filter(|(n, r)| n.node_type == NodeType::Interaction && *r == Relation::InteractedWith)
            .map(|(n, _)| json!({"id": &n.id, "title": &n.title, "date": &n.created_at, "summary": truncate_str(&n.content, 200)}))
            .collect();

        Ok(ToolResult {
            success: true,
            output: json!({
                "client": {"id": &client.id, "name": &client.title, "tags": &client.tags},
                "contacts": contacts,
                "interactions": interactions,
                "contact_count": contacts.len(),
                "interaction_count": interactions.len(),
            })
            .to_string(),
            error: None,
        })
    }

    fn handle_interaction_log(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let client_id = args
            .get("client_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'client_id' for interaction_log"))?;

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(20);

        let outbound = self.graph.find_related(client_id)?;
        let mut interactions: Vec<_> = outbound
            .into_iter()
            .filter(|(n, r)| n.node_type == NodeType::Interaction && *r == Relation::InteractedWith)
            .map(|(n, _)| n)
            .collect();

        // Sort by created_at descending (most recent first)
        interactions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        interactions.truncate(limit);

        let entries: Vec<_> = interactions
            .iter()
            .map(|n| {
                json!({
                    "id": &n.id,
                    "title": &n.title,
                    "content": &n.content,
                    "date": &n.created_at,
                    "tags": &n.tags,
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: json!({"interactions": entries, "count": entries.len()}).to_string(),
            error: None,
        })
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{truncated}...")
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
