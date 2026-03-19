//! Graph hot-nodes tool — surface the most recently active and relevant nodes.
//!
//! Requires the `memory-graph` feature (CozoDB). When disabled or when no graph
//! database is configured, the tool returns a descriptive error.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
#[cfg(feature = "memory-graph")]
use std::sync::Arc;

const VALID_NODE_TYPES: &[&str] = &["concept", "fact", "episode", "hypothesis"];

/// List the hottest (most recently active and relevant) nodes in the knowledge graph.
pub struct GraphHotNodesTool {
    #[cfg(feature = "memory-graph")]
    graph_db: Option<Arc<cozo::DbInstance>>,
}

impl GraphHotNodesTool {
    /// Create a new `GraphHotNodesTool` with access to the CozoDB graph database.
    #[cfg(feature = "memory-graph")]
    pub fn new(graph_db: Option<Arc<cozo::DbInstance>>) -> Self {
        Self { graph_db }
    }

    /// Create a new `GraphHotNodesTool` (no-op when `memory-graph` feature is disabled).
    #[cfg(not(feature = "memory-graph"))]
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for GraphHotNodesTool {
    fn name(&self) -> &str {
        "graph_hot_nodes"
    }

    fn description(&self) -> &str {
        "List the hottest (most recently active and relevant) nodes in the knowledge graph."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "threshold": {
                    "type": "number",
                    "description": "Minimum heat value to include (0.0-1.0). Defaults to 0.3."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of nodes to return. Defaults to 20."
                },
                "node_type": {
                    "type": "string",
                    "description": "Filter by node type: concept, fact, episode, hypothesis.",
                    "enum": ["concept", "fact", "episode", "hypothesis"]
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        #[cfg(not(feature = "memory-graph"))]
        {
            let _ = args;
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Knowledge graph unavailable: compile with --features memory-graph".into(),
                ),
            });
        }

        #[cfg(feature = "memory-graph")]
        {
            let db: &Arc<cozo::DbInstance> = match &self.graph_db {
                Some(db) => db,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Knowledge graph database not configured".into()),
                    });
                }
            };

            let threshold = args
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3);

            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);

            let node_type_filter = args.get("node_type").and_then(|v| v.as_str());

            if let Some(nt) = node_type_filter {
                if !VALID_NODE_TYPES.contains(&nt) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Invalid node_type '{}'. Must be one of: {}",
                            nt,
                            VALID_NODE_TYPES.join(", ")
                        )),
                    });
                }
            }

            let query = if let Some(nt) = node_type_filter {
                format!(
                    r#"?[id, name, content, heat, node_type] :=
                        *knowledge_node{{id, name, content, heat, node_type}},
                        heat >= {threshold},
                        node_type == "{nt}"
                    :order -heat
                    :limit {limit}"#,
                )
            } else {
                format!(
                    r#"?[id, name, content, heat, node_type] :=
                        *knowledge_node{{id, name, content, heat, node_type}},
                        heat >= {threshold}
                    :order -heat
                    :limit {limit}"#,
                )
            };

            match db.run_script(
                &query,
                std::collections::BTreeMap::default(),
                cozo::ScriptMutability::Immutable,
            ) {
                Ok(result) => {
                    let headers = &result.headers;
                    let mut nodes = Vec::new();

                    for row in &result.rows {
                        let mut node = serde_json::Map::new();
                        for (i, header) in headers.iter().enumerate() {
                            if let Some(val) = row.get(i) {
                                node.insert(header.clone(), cozo_val_to_json(val));
                            }
                        }
                        nodes.push(serde_json::Value::Object(node));
                    }

                    Ok(ToolResult {
                        success: true,
                        output: json!({
                            "nodes": nodes,
                            "count": nodes.len(),
                            "threshold": threshold,
                            "limit": limit,
                        })
                        .to_string(),
                        error: None,
                    })
                }
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to query hot nodes: {e}")),
                }),
            }
        }
    }
}

/// Convert a CozoDB `DataValue` to a `serde_json::Value`.
#[cfg(feature = "memory-graph")]
fn cozo_val_to_json(val: &cozo::DataValue) -> serde_json::Value {
    match val {
        cozo::DataValue::Null => serde_json::Value::Null,
        cozo::DataValue::Bool(b) => json!(*b),
        cozo::DataValue::Num(n) => match n {
            cozo::Num::Int(i) => json!(*i),
            cozo::Num::Float(f) => json!(*f),
        },
        cozo::DataValue::Str(s) => json!(AsRef::<str>::as_ref(s)),
        cozo::DataValue::List(items) => {
            let vals: Vec<serde_json::Value> = items.iter().map(cozo_val_to_json).collect();
            json!(vals)
        }
        _ => json!(format!("{val:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_schema() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphHotNodesTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphHotNodesTool::new();

        assert_eq!(tool.name(), "graph_hot_nodes");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["threshold"].is_object());
        assert!(schema["properties"]["limit"].is_object());
        assert!(schema["properties"]["node_type"].is_object());
        // No required parameters
        assert!(schema.get("required").is_none());
    }

    #[tokio::test]
    async fn returns_error_when_no_db() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphHotNodesTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphHotNodesTool::new();

        let result = tool.execute(json!({})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn returns_error_for_invalid_node_type() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphHotNodesTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphHotNodesTool::new();

        let result = tool
            .execute(json!({"node_type": "invalid_type"}))
            .await
            .unwrap();

        assert!(!result.success);
        // When feature is disabled, the error is about the feature, not the node type
        assert!(result.error.is_some());
    }

    #[test]
    fn valid_node_types_list() {
        assert_eq!(VALID_NODE_TYPES.len(), 4);
        assert!(VALID_NODE_TYPES.contains(&"concept"));
        assert!(VALID_NODE_TYPES.contains(&"fact"));
        assert!(VALID_NODE_TYPES.contains(&"episode"));
        assert!(VALID_NODE_TYPES.contains(&"hypothesis"));
    }
}
