//! Knowledge management tool for capturing, searching, and reusing expertise.
//! Exposes the knowledge graph to the agent via the `Tool` trait.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_memory::knowledge_graph::{KnowledgeGraph, NodeType, Relation};

const CLIENT_NETWORK_INTERACTION_LIMIT: usize = 20;
const CLIENT_NETWORK_ENTITY_LIMIT: usize = 100;
const DEFAULT_GRAPH_NEIGHBOR_LIMIT: usize = 25;
const DEFAULT_INTERACTION_LOG_LIMIT: usize = 20;
const MAX_KNOWLEDGE_RESULT_LIMIT: usize = 100;

const KNOWLEDGE_ACTIONS: &[&str] = &[
    "capture",
    "search",
    "relate",
    "suggest",
    "expert_find",
    "lessons_extract",
    "graph_stats",
    "graph_neighbors",
    "client_network",
    "interaction_log",
];

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
        "Manage a knowledge graph of architecture decisions, solution patterns, lessons learned, experts, and relationship links."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": KNOWLEDGE_ACTIONS,
                    "description": "The action to perform"
                },
                "node_type": {
                    "type": "string",
                    "enum": NodeType::schema_values(),
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
                    "enum": Relation::schema_values(),
                    "description": "Relationship type (for relate)"
                },
                "node_id": {
                    "type": "string",
                    "description": "Knowledge node ID (for graph_neighbors)"
                },
                "client_id": {
                    "type": "string",
                    "description": "Client node ID (for client_network, interaction_log)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_KNOWLEDGE_RESULT_LIMIT,
                    "description": "Maximum number of results to return (for graph_neighbors, interaction_log)"
                },
                "filters": {
                    "type": "object",
                    "properties": {
                        "node_type": { "type": "string", "enum": NodeType::schema_values() },
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
        let action = args.get("action").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "action"})),
                "knowledge_tool: missing action parameter"
            );
            anyhow::Error::msg("missing 'action' parameter")
        })?;

        match action {
            "capture" => self.handle_capture(&args),
            "search" => self.handle_search(&args),
            "relate" => self.handle_relate(&args),
            "suggest" => self.handle_suggest(&args),
            "expert_find" => self.handle_expert_find(&args),
            "lessons_extract" => self.handle_lessons_extract(&args),
            "graph_stats" => self.handle_graph_stats(),
            "graph_neighbors" => self.handle_graph_neighbors(&args),
            "client_network" => self.handle_client_network(&args),
            "interaction_log" => self.handle_interaction_log(&args),
            other => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
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
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "action": "capture",
                            "param": "node_type",
                        })),
                    "knowledge_tool: capture missing node_type"
                );
                anyhow::Error::msg("missing 'node_type' for capture")
            })?;
        let title = args.get("title").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "action": "capture",
                        "param": "title",
                    })),
                "knowledge_tool: capture missing title"
            );
            anyhow::Error::msg("missing 'title' for capture")
        })?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "action": "capture",
                            "param": "content",
                        })),
                    "knowledge_tool: capture missing content"
                );
                anyhow::Error::msg("missing 'content' for capture")
            })?;

        let node_type = NodeType::parse(node_type_str).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "node_type": node_type_str,
                        "error": format!("{}", e),
                    })),
                "knowledge_tool: invalid node_type"
            );
            anyhow::Error::msg(format!("{e}"))
        })?;

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
                output: json!({ "node_id": id }).to_string().into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
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
            output: json!({ "results": results, "count": results.len() })
                .to_string()
                .into(),
            error: None,
        })
    }

    fn handle_relate(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let from_id = args
            .get("from_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "action": "relate",
                            "param": "from_id",
                        })),
                    "knowledge_tool: relate missing from_id"
                );
                anyhow::Error::msg("missing 'from_id' for relate")
            })?;
        let to_id = args.get("to_id").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "action": "relate",
                        "param": "to_id",
                    })),
                "knowledge_tool: relate missing to_id"
            );
            anyhow::Error::msg("missing 'to_id' for relate")
        })?;
        let relation_str = args
            .get("relation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "action": "relate",
                            "param": "relation",
                        })),
                    "knowledge_tool: relate missing relation"
                );
                anyhow::Error::msg("missing 'relation' for relate")
            })?;

        let relation = Relation::parse(relation_str).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "relation": relation_str,
                        "error": format!("{}", e),
                    })),
                "knowledge_tool: invalid relation"
            );
            anyhow::Error::msg(format!("{e}"))
        })?;

        match self.graph.add_edge(from_id, to_id, relation) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: "relationship created".to_string().into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("relate failed: {e}")),
            }),
        }
    }

    fn handle_suggest(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .or_else(|| args.get("content"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "action": "suggest",
                            "missing": "query_or_content",
                        })),
                    "knowledge_tool: suggest missing query/content"
                );
                anyhow::Error::msg("missing 'query' or 'content' for suggest")
            })?;

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
            output: json!({ "suggestions": suggestions, "count": suggestions.len() })
                .to_string()
                .into(),
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
                output: ToolOutput::default(),
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
            output: json!({ "experts": output, "count": output.len() })
                .to_string()
                .into(),
            error: None,
        })
    }

    fn handle_lessons_extract(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let text = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "action": "lessons_extract",
                            "param": "content",
                        })),
                    "knowledge_tool: lessons_extract missing content"
                );
                anyhow::Error::msg("missing 'content' for lessons_extract")
            })?;

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
            output: json!({ "lessons": lessons, "count": lessons.len() })
                .to_string()
                .into(),
            error: None,
        })
    }

    fn handle_graph_stats(&self) -> anyhow::Result<ToolResult> {
        match self.graph.stats() {
            Ok(stats) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string(&stats).unwrap_or_default().into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("failed to get stats: {e}")),
            }),
        }
    }

    fn handle_graph_neighbors(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let node_id = match args.get("node_id").and_then(|v| v.as_str()) {
            Some(node_id) => node_id,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("missing 'node_id' for graph_neighbors".into()),
                });
            }
        };
        let limit = bounded_limit(args, DEFAULT_GRAPH_NEIGHBOR_LIMIT);

        let root = match self.graph.get_node(node_id)? {
            Some(root) => root,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("node not found: {node_id}")),
                });
            }
        };

        let outbound = self.graph.find_outbound(node_id, limit)?;
        let inbound = self.graph.find_inbound(node_id, limit)?;

        let outbound: Vec<_> = outbound
            .iter()
            .map(|(node, relation)| neighbor_json(node, *relation))
            .collect();
        let inbound: Vec<_> = inbound
            .iter()
            .map(|(node, relation)| neighbor_json(node, *relation))
            .collect();
        let outbound_count = outbound.len();
        let inbound_count = inbound.len();

        Ok(ToolResult {
            success: true,
            output: json!({
                "node": {
                    "id": &root.id,
                    "type": root.node_type,
                    "title": &root.title,
                    "tags": &root.tags,
                },
                "outbound": outbound,
                "inbound": inbound,
                "outbound_count": outbound_count,
                "inbound_count": inbound_count,
            })
            .to_string()
            .into(),
            error: None,
        })
    }

    fn handle_client_network(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let client = match self.client_node(args, "client_network") {
            Ok(client) => client,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };
        let contacts = self.graph.find_inbound_by_relation_and_type(
            &client.id,
            Relation::ContactOf,
            NodeType::Contact,
            CLIENT_NETWORK_ENTITY_LIMIT,
        )?;
        let managers = self.graph.find_inbound_by_relation_and_type(
            &client.id,
            Relation::ManagesClient,
            NodeType::Expert,
            CLIENT_NETWORK_ENTITY_LIMIT,
        )?;

        let contacts: Vec<_> = contacts
            .iter()
            .map(|node| json!({"id": &node.id, "name": &node.title, "tags": &node.tags}))
            .collect();

        let managers: Vec<_> = managers
            .iter()
            .map(|node| {
                json!({
                    "id": &node.id,
                    "type": node.node_type,
                    "name": &node.title,
                    "tags": &node.tags,
                })
            })
            .collect();

        let interactions =
            self.client_interactions(&client.id, CLIENT_NETWORK_INTERACTION_LIMIT)?;

        let interactions: Vec<_> = interactions
            .iter()
            .map(|node| {
                json!({
                    "id": &node.id,
                    "title": &node.title,
                    "date": node.created_at,
                    "summary": truncate_str(&node.content, 200),
                    "tags": &node.tags,
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: json!({
                "client": {"id": &client.id, "name": &client.title, "tags": &client.tags},
                "contacts": contacts,
                "managers": managers,
                "interactions": interactions,
                "contact_count": contacts.len(),
                "manager_count": managers.len(),
                "interaction_count": interactions.len(),
            })
            .to_string()
            .into(),
            error: None,
        })
    }

    fn handle_interaction_log(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let client = match self.client_node(args, "interaction_log") {
            Ok(client) => client,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };
        let limit = bounded_limit(args, DEFAULT_INTERACTION_LOG_LIMIT);

        let interactions = self.client_interactions(&client.id, limit)?;

        let entries: Vec<_> = interactions
            .iter()
            .map(|node| {
                json!({
                    "id": &node.id,
                    "title": &node.title,
                    "content": &node.content,
                    "date": node.created_at,
                    "tags": &node.tags,
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: json!({"interactions": entries, "count": entries.len()})
                .to_string()
                .into(),
            error: None,
        })
    }

    fn client_node(
        &self,
        args: &serde_json::Value,
        action: &'static str,
    ) -> anyhow::Result<zeroclaw_memory::knowledge_graph::KnowledgeNode> {
        let client_id = args
            .get("client_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "action": action,
                            "param": "client_id",
                        })),
                    "knowledge_tool: client action missing client_id"
                );
                anyhow::Error::msg(format!("missing 'client_id' for {action}"))
            })?;

        let Some(client) = self.graph.get_node(client_id)? else {
            anyhow::bail!("client node not found: {client_id}");
        };
        if client.node_type != NodeType::Client {
            anyhow::bail!(
                "node {} is not a client (is {})",
                client_id,
                client.node_type.as_str()
            );
        }
        Ok(client)
    }

    fn client_interactions(
        &self,
        client_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<zeroclaw_memory::knowledge_graph::KnowledgeNode>> {
        self.graph.find_outbound_by_relation_and_type(
            client_id,
            Relation::InteractedWith,
            NodeType::Interaction,
            limit,
        )
    }
}

fn neighbor_json(
    node: &zeroclaw_memory::knowledge_graph::KnowledgeNode,
    relation: Relation,
) -> serde_json::Value {
    json!({
        "id": &node.id,
        "type": node.node_type,
        "title": &node.title,
        "relation": relation,
        "content_preview": truncate_str(&node.content, 200),
        "tags": &node.tags,
    })
}

fn bounded_limit(args: &serde_json::Value, default: usize) -> usize {
    let default = default.clamp(1, MAX_KNOWLEDGE_RESULT_LIMIT);
    args.get("limit")
        .and_then(|v| v.as_u64())
        .map(|limit| {
            usize::try_from(limit)
                .unwrap_or(MAX_KNOWLEDGE_RESULT_LIMIT)
                .clamp(1, MAX_KNOWLEDGE_RESULT_LIMIT)
        })
        .unwrap_or(default)
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
    use tempfile::TempDir;
    use zeroclaw_memory::knowledge_graph::KnowledgeGraph;

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
        assert!(tool.description().contains("relationship links"));
        assert!(!tool.description().contains("graph_neighbors"));
        assert!(!tool.description().contains("client_network"));

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(actions.len(), KNOWLEDGE_ACTIONS.len());
        for action in KNOWLEDGE_ACTIONS {
            assert!(actions.contains(&json!(action)));
        }
        assert!(schema["properties"]["node_id"].is_object());
        assert_eq!(
            schema["properties"]["limit"]["maximum"],
            MAX_KNOWLEDGE_RESULT_LIMIT
        );
        assert!(
            schema["properties"]["node_type"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("client"))
        );
        assert!(
            schema["properties"]["relation"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("contact_of"))
        );
    }

    #[tokio::test]
    async fn graph_neighbors_returns_generic_inbound_and_outbound_edges() {
        let (_tmp, tool) = test_tool();

        let pattern_id = capture_node(
            &tool,
            "pattern",
            "Event sourcing",
            "Persist events and rebuild state from them",
        )
        .await;
        let tech_id = capture_node(&tool, "technology", "Kafka", "Event log technology").await;
        let lesson_id = capture_node(
            &tool,
            "lesson",
            "Audit requirements",
            "Audit-heavy systems need durable event trails",
        )
        .await;

        relate_nodes(&tool, &pattern_id, &tech_id, "uses").await;
        relate_nodes(&tool, &lesson_id, &pattern_id, "applies_to").await;

        let result = tool
            .execute(json!({
                "action": "graph_neighbors",
                "node_id": pattern_id,
                "limit": 10
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["node"]["title"], "Event sourcing");
        assert_eq!(output["outbound_count"], 1);
        assert_eq!(output["inbound_count"], 1);
        assert_eq!(output["outbound"][0]["title"], "Kafka");
        assert_eq!(output["outbound"][0]["relation"], "uses");
        assert_eq!(output["inbound"][0]["title"], "Audit requirements");
        assert_eq!(output["inbound"][0]["relation"], "applies_to");
    }

    #[tokio::test]
    async fn graph_neighbors_orders_and_limits_each_direction_in_storage() {
        let (_tmp, tool) = test_tool();

        let pattern_id = capture_node(
            &tool,
            "pattern",
            "Event sourcing",
            "Persist events and rebuild state from them",
        )
        .await;
        let old_outbound =
            capture_node(&tool, "technology", "Old queue", "First outbound neighbor").await;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let new_outbound = capture_node(
            &tool,
            "technology",
            "New stream",
            "Latest outbound neighbor",
        )
        .await;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let old_inbound =
            capture_node(&tool, "lesson", "Old lesson", "First inbound neighbor").await;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let new_inbound =
            capture_node(&tool, "lesson", "New lesson", "Latest inbound neighbor").await;

        relate_nodes(&tool, &pattern_id, &old_outbound, "uses").await;
        relate_nodes(&tool, &pattern_id, &new_outbound, "uses").await;
        relate_nodes(&tool, &old_inbound, &pattern_id, "applies_to").await;
        relate_nodes(&tool, &new_inbound, &pattern_id, "applies_to").await;

        let result = tool
            .execute(json!({
                "action": "graph_neighbors",
                "node_id": pattern_id,
                "limit": 1
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["outbound_count"], 1);
        assert_eq!(output["inbound_count"], 1);
        assert_eq!(output["outbound"][0]["title"], "New stream");
        assert_eq!(output["inbound"][0]["title"], "New lesson");
    }

    #[tokio::test]
    async fn client_network_returns_contacts_managers_and_interactions() {
        let (_tmp, tool) = test_tool();

        let client_id = capture_node(
            &tool,
            "client",
            "Example Account",
            "Enterprise account for relationship tracking",
        )
        .await;
        let contact_id = capture_node(&tool, "contact", "Contact Alpha", "Technical contact").await;
        let manager_id = capture_node(&tool, "expert", "Expert Alpha", "Relationship owner").await;
        let interaction_id = capture_node(
            &tool,
            "interaction",
            "Discovery call",
            "Discussed integration requirements",
        )
        .await;

        relate_nodes(&tool, &contact_id, &client_id, "contact_of").await;
        relate_nodes(&tool, &manager_id, &client_id, "manages_client").await;
        relate_nodes(&tool, &client_id, &interaction_id, "interacted_with").await;

        let result = tool
            .execute(json!({
                "action": "client_network",
                "client_id": client_id
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["client"]["name"], "Example Account");
        assert_eq!(output["contact_count"], 1);
        assert_eq!(output["manager_count"], 1);
        assert_eq!(output["interaction_count"], 1);
        assert_eq!(output["contacts"][0]["name"], "Contact Alpha");
        assert_eq!(output["managers"][0]["name"], "Expert Alpha");
        assert_eq!(output["interactions"][0]["title"], "Discovery call");
    }

    #[tokio::test]
    async fn client_network_ignores_malformed_manager_edges() {
        let (_tmp, tool) = test_tool();

        let client_id = capture_node(&tool, "client", "Example Account", "Account").await;
        let interaction_id =
            capture_node(&tool, "interaction", "Status call", "Not a manager").await;

        relate_nodes(&tool, &interaction_id, &client_id, "manages_client").await;

        let result = tool
            .execute(json!({
                "action": "client_network",
                "client_id": client_id
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["manager_count"], 0);
        assert!(output["managers"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn interaction_log_sorts_recent_first_and_respects_limit() {
        let (_tmp, tool) = test_tool();

        let client_id = capture_node(&tool, "client", "Example Account", "Account").await;
        let first_id = capture_node(&tool, "interaction", "First call", "Initial discussion").await;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second_id = capture_node(&tool, "interaction", "Second call", "Follow-up").await;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let third_id = capture_node(&tool, "interaction", "Third call", "Next step").await;

        relate_nodes(&tool, &client_id, &first_id, "interacted_with").await;
        relate_nodes(&tool, &client_id, &second_id, "interacted_with").await;
        relate_nodes(&tool, &client_id, &third_id, "interacted_with").await;

        let result = tool
            .execute(json!({
                "action": "interaction_log",
                "client_id": client_id,
                "limit": 2
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["count"], 2);
        assert_eq!(output["interactions"][0]["title"], "Third call");
        assert_eq!(output["interactions"][1]["title"], "Second call");
    }

    #[test]
    fn bounded_limit_clamps_user_values() {
        assert_eq!(
            bounded_limit(&json!({}), DEFAULT_INTERACTION_LOG_LIMIT),
            DEFAULT_INTERACTION_LOG_LIMIT
        );
        assert_eq!(
            bounded_limit(&json!({ "limit": 0 }), DEFAULT_INTERACTION_LOG_LIMIT),
            1
        );
        assert_eq!(
            bounded_limit(&json!({ "limit": u64::MAX }), DEFAULT_INTERACTION_LOG_LIMIT),
            MAX_KNOWLEDGE_RESULT_LIMIT
        );
    }

    #[tokio::test]
    async fn client_actions_reject_non_client_node() {
        let (_tmp, tool) = test_tool();
        let pattern_id = capture_node(&tool, "pattern", "Pattern Alpha", "Not a client").await;

        let result = tool
            .execute(json!({
                "action": "client_network",
                "client_id": pattern_id
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not a client"));
    }

    async fn capture_node(
        tool: &KnowledgeTool,
        node_type: &str,
        title: &str,
        content: &str,
    ) -> String {
        let result = tool
            .execute(json!({
                "action": "capture",
                "node_type": node_type,
                "title": title,
                "content": content,
            }))
            .await
            .unwrap();
        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        output["node_id"].as_str().unwrap().to_string()
    }

    async fn relate_nodes(tool: &KnowledgeTool, from_id: &str, to_id: &str, relation: &str) {
        let result = tool
            .execute(json!({
                "action": "relate",
                "from_id": from_id,
                "to_id": to_id,
                "relation": relation,
            }))
            .await
            .unwrap();
        assert!(result.success);
    }
}
