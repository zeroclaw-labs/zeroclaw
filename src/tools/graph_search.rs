//! Graph search tool — keyword search across all knowledge graph node types.
//!
//! Requires the `memory-graph` feature (CozoDB). When disabled or when no graph
//! database is configured, the tool returns a descriptive error.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
#[cfg(feature = "memory-graph")]
use std::sync::Arc;

const VALID_NODE_TYPES: &[&str] = &["concept", "fact", "episode", "hypothesis"];

/// Search the knowledge graph using keyword matching across all node types.
pub struct GraphSearchTool {
    #[cfg(feature = "memory-graph")]
    graph_db: Option<Arc<cozo::DbInstance>>,
}

impl GraphSearchTool {
    /// Create a new `GraphSearchTool` with access to the CozoDB graph database.
    #[cfg(feature = "memory-graph")]
    pub fn new(graph_db: Option<Arc<cozo::DbInstance>>) -> Self {
        Self { graph_db }
    }

    /// Create a new `GraphSearchTool` (no-op when `memory-graph` feature is disabled).
    #[cfg(not(feature = "memory-graph"))]
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for GraphSearchTool {
    fn name(&self) -> &str {
        "graph_search"
    }

    fn description(&self) -> &str {
        "Search the knowledge graph using keyword matching across all node types."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search text to match against node content and name fields"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return. Defaults to 10."
                },
                "node_type": {
                    "type": "string",
                    "description": "Filter by node type: concept, fact, episode, hypothesis.",
                    "enum": ["concept", "fact", "episode", "hypothesis"]
                }
            },
            "required": ["query"]
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

            let query_text = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;

            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);

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

            // Escape double quotes in the query text to prevent injection
            let escaped_query = query_text.replace('\\', "\\\\").replace('"', "\\\"");

            let datalog_query = if let Some(nt) = node_type_filter {
                format!(
                    r#"?[id, name, content, heat, node_type] :=
                        *knowledge_node{{id, name, content, heat, node_type}},
                        node_type == "{nt}",
                        (contains(content, "{escaped_query}") || contains(name, "{escaped_query}"))
                    :order -heat
                    :limit {limit}"#,
                )
            } else {
                format!(
                    r#"?[id, name, content, heat, node_type] :=
                        *knowledge_node{{id, name, content, heat, node_type}},
                        (contains(content, "{escaped_query}") || contains(name, "{escaped_query}"))
                    :order -heat
                    :limit {limit}"#,
                )
            };

            match db.run_script(
                &datalog_query,
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
                            "results": nodes,
                            "count": nodes.len(),
                            "query": query_text,
                            "limit": limit,
                        })
                        .to_string(),
                        error: None,
                    })
                }
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to search knowledge graph: {e}")),
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
        let tool = GraphSearchTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphSearchTool::new();

        assert_eq!(tool.name(), "graph_search");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["limit"].is_object());
        assert!(schema["properties"]["node_type"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("query")));
    }

    #[tokio::test]
    async fn returns_error_when_no_db() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphSearchTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphSearchTool::new();

        let result = tool.execute(json!({"query": "test search"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn missing_query_returns_error() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphSearchTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphSearchTool::new();

        let result = tool.execute(json!({})).await;

        // When feature is disabled, it returns Ok with error field set.
        // When feature is enabled with no db, it returns Ok with error field set.
        // Both paths are valid - the tool gracefully handles missing params.
        if let Ok(r) = result {
            assert!(!r.success);
        }
        // anyhow error from missing param is also acceptable
    }

    #[tokio::test]
    async fn returns_error_for_invalid_node_type() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphSearchTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphSearchTool::new();

        let result = tool
            .execute(json!({"query": "test", "node_type": "bogus"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn valid_node_types_list() {
        assert_eq!(VALID_NODE_TYPES.len(), 4);
        assert!(VALID_NODE_TYPES.contains(&"concept"));
        assert!(VALID_NODE_TYPES.contains(&"hypothesis"));
    }
}
