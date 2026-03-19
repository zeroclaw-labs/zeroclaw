//! Graph connect tool — create epistemic relations between knowledge graph nodes.
//!
//! Requires the `memory-graph` feature (CozoDB). When disabled or when no graph
//! database is configured, the tool returns a descriptive error.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
#[cfg(feature = "memory-graph")]
use std::sync::Arc;

/// Create an epistemic relation between two nodes in the knowledge graph.
pub struct GraphConnectTool {
    #[cfg(feature = "memory-graph")]
    graph_db: Option<Arc<cozo::DbInstance>>,
}

impl GraphConnectTool {
    /// Create a new `GraphConnectTool` with access to the CozoDB graph database.
    #[cfg(feature = "memory-graph")]
    pub fn new(graph_db: Option<Arc<cozo::DbInstance>>) -> Self {
        Self { graph_db }
    }

    /// Create a new `GraphConnectTool` (no-op when `memory-graph` feature is disabled).
    #[cfg(not(feature = "memory-graph"))]
    pub fn new() -> Self {
        Self {}
    }
}

const VALID_RELATION_TYPES: &[&str] = &[
    "related",
    "supports",
    "contradicts",
    "derived_from",
    "is_a",
    "part_of",
    "causes",
];

#[async_trait]
impl Tool for GraphConnectTool {
    fn name(&self) -> &str {
        "graph_connect"
    }

    fn description(&self) -> &str {
        "Create an epistemic relation between two nodes in the knowledge graph."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "from_id": {
                    "type": "string",
                    "description": "Source node ID"
                },
                "to_id": {
                    "type": "string",
                    "description": "Target node ID"
                },
                "relation_type": {
                    "type": "string",
                    "description": "Type of relation: related, supports, contradicts, derived_from, is_a, part_of, causes. Defaults to 'related'.",
                    "enum": ["related", "supports", "contradicts", "derived_from", "is_a", "part_of", "causes"]
                },
                "weight": {
                    "type": "number",
                    "description": "Relation weight (0.0-1.0). Defaults to 1.0."
                }
            },
            "required": ["from_id", "to_id"]
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

            let from_id = args
                .get("from_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'from_id' parameter"))?;

            let to_id = args
                .get("to_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'to_id' parameter"))?;

            let relation_type = args
                .get("relation_type")
                .and_then(|v| v.as_str())
                .unwrap_or("related");

            if !VALID_RELATION_TYPES.contains(&relation_type) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid relation_type '{}'. Must be one of: {}",
                        relation_type,
                        VALID_RELATION_TYPES.join(", ")
                    )),
                });
            }

            let weight = args.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0);

            let created_at = chrono::Utc::now().to_rfc3339();

            let query = format!(
                r#"?[from_id, to_id, relation_type, weight, created_at] <- [
                    ["{from_id}", "{to_id}", "{relation_type}", {weight}, "{created_at}"]
                ]
                :put relates_to {{from_id, to_id => relation_type, weight, created_at}}"#,
            );

            match db.run_script(&query, std::collections::BTreeMap::default(), cozo::ScriptMutability::Mutable) {
                Ok(_) => Ok(ToolResult {
                    success: true,
                    output: json!({
                        "from_id": from_id,
                        "to_id": to_id,
                        "relation_type": relation_type,
                        "weight": weight,
                        "created_at": created_at,
                        "message": format!(
                            "Created '{relation_type}' relation from '{from_id}' to '{to_id}' (weight={weight})"
                        )
                    })
                    .to_string(),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to create relation: {e}")),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_schema() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphConnectTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphConnectTool::new();

        assert_eq!(tool.name(), "graph_connect");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["from_id"].is_object());
        assert!(schema["properties"]["to_id"].is_object());
        assert!(schema["properties"]["relation_type"].is_object());
        assert!(schema["properties"]["weight"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("from_id")));
        assert!(required.contains(&json!("to_id")));
    }

    #[tokio::test]
    async fn returns_error_when_no_db() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphConnectTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphConnectTool::new();

        let result = tool
            .execute(json!({
                "from_id": "node_a",
                "to_id": "node_b"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn valid_relation_types_list() {
        assert_eq!(VALID_RELATION_TYPES.len(), 7);
        assert!(VALID_RELATION_TYPES.contains(&"related"));
        assert!(VALID_RELATION_TYPES.contains(&"causes"));
        assert!(VALID_RELATION_TYPES.contains(&"contradicts"));
    }
}
