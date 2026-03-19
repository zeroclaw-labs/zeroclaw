//! Graph add-concept tool — insert a concept node into the knowledge graph.
//!
//! Requires the `memory-graph` feature (CozoDB backend). When the feature is
//! disabled at compile time, the tool returns a descriptive error.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
#[cfg(feature = "memory-graph")]
use std::sync::Arc;

/// Add a concept node to the CozoDB knowledge graph.
pub struct GraphAddConceptTool {
    #[cfg(feature = "memory-graph")]
    graph_db: Option<Arc<cozo::DbInstance>>,
}

impl GraphAddConceptTool {
    /// Create a new `GraphAddConceptTool`.
    ///
    /// When `memory-graph` is enabled, accepts an optional `Arc<cozo::DbInstance>`.
    /// When the feature is disabled, the constructor takes no arguments and the
    /// tool always returns an error at execution time.
    #[cfg(feature = "memory-graph")]
    pub fn new(graph_db: Option<Arc<cozo::DbInstance>>) -> Self {
        Self { graph_db }
    }

    #[cfg(not(feature = "memory-graph"))]
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Tool for GraphAddConceptTool {
    fn name(&self) -> &str {
        "graph_add_concept"
    }

    fn description(&self) -> &str {
        "Add a new concept node to the knowledge graph with name, description, and category."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the concept"
                },
                "description": {
                    "type": "string",
                    "description": "Description of the concept"
                },
                "category": {
                    "type": "string",
                    "description": "Category for the concept (default: 'general')"
                }
            },
            "required": ["name", "description"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        #[cfg(not(feature = "memory-graph"))]
        {
            let _ = args;
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Graph memory requires `memory-graph` feature".into()),
            });
        }

        #[cfg(feature = "memory-graph")]
        {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;

            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'description' parameter"))?;

            let category = args
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("general");

            let db: &Arc<cozo::DbInstance> = match &self.graph_db {
                Some(db) => db,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("No graph database instance available".into()),
                    });
                }
            };

            let id = uuid::Uuid::new_v4().to_string();
            let timestamp = chrono::Utc::now().to_rfc3339();

            let query = r#":put concept {
                id: $id,
                name: $name,
                description: $description,
                category: $category,
                created_at: $created_at
            }"#;

            let params = std::collections::BTreeMap::from([
                ("id".into(), cozo::DataValue::Str(id.clone().into())),
                ("name".into(), cozo::DataValue::Str(name.into())),
                (
                    "description".into(),
                    cozo::DataValue::Str(description.into()),
                ),
                ("category".into(), cozo::DataValue::Str(category.into())),
                ("created_at".into(), cozo::DataValue::Str(timestamp.into())),
            ]);

            match db.run_script(query, params, cozo::ScriptMutability::Mutable) {
                Ok(_) => Ok(ToolResult {
                    success: true,
                    output: format!("Concept '{name}' added to knowledge graph with id {id}"),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to add concept: {e}")),
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
        let tool = GraphAddConceptTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphAddConceptTool::new();

        assert_eq!(tool.name(), "graph_add_concept");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["name"].is_object());
        assert!(schema["properties"]["description"].is_object());
        assert!(schema["properties"]["category"].is_object());
    }

    #[tokio::test]
    async fn execute_without_feature_or_db() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphAddConceptTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphAddConceptTool::new();

        let result = tool
            .execute(json!({
                "name": "Rust",
                "description": "A systems programming language"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }
}
