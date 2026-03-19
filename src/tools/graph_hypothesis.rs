//! Graph hypothesis tool — create a hypothesis in the knowledge graph for later validation.
//!
//! Requires the `memory-graph` feature (CozoDB backend). When the feature is
//! disabled at compile time, the tool returns a descriptive error.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
#[cfg(feature = "memory-graph")]
use std::sync::Arc;

/// Create a hypothesis in the CozoDB knowledge graph.
pub struct GraphHypothesisTool {
    #[cfg(feature = "memory-graph")]
    graph_db: Option<Arc<cozo::DbInstance>>,
}

impl GraphHypothesisTool {
    /// Create a new `GraphHypothesisTool`.
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
impl Tool for GraphHypothesisTool {
    fn name(&self) -> &str {
        "graph_hypothesis"
    }

    fn description(&self) -> &str {
        "Create a hypothesis in the knowledge graph for later validation."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "claim": {
                    "type": "string",
                    "description": "The hypothesis claim to record"
                },
                "evidence_for": {
                    "type": "string",
                    "description": "Optional supporting evidence for the hypothesis"
                },
                "confidence": {
                    "type": "number",
                    "description": "Confidence level between 0.0 and 1.0 (default: 0.5)"
                }
            },
            "required": ["claim"]
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
            let claim = args
                .get("claim")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'claim' parameter"))?;

            let evidence_for = args
                .get("evidence_for")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let confidence = args
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5);

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

            let query = r#":put hypothesis {
                id: $id,
                claim: $claim,
                evidence_for: $evidence_for,
                confidence: $confidence,
                status: $status,
                created_at: $created_at,
                updated_at: $updated_at
            }"#;

            let params = std::collections::BTreeMap::from([
                ("id".into(), cozo::DataValue::Str(id.clone().into())),
                ("claim".into(), cozo::DataValue::Str(claim.into())),
                (
                    "evidence_for".into(),
                    cozo::DataValue::Str(evidence_for.into()),
                ),
                ("confidence".into(), cozo::DataValue::from(confidence)),
                ("status".into(), cozo::DataValue::Str("open".into())),
                (
                    "created_at".into(),
                    cozo::DataValue::Str(timestamp.clone().into()),
                ),
                ("updated_at".into(), cozo::DataValue::Str(timestamp.into())),
            ]);

            match db.run_script(query, params, cozo::ScriptMutability::Mutable) {
                Ok(_) => Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Hypothesis created with id {id} (confidence: {confidence:.2})"
                    ),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to create hypothesis: {e}")),
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
        let tool = GraphHypothesisTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphHypothesisTool::new();

        assert_eq!(tool.name(), "graph_hypothesis");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["claim"].is_object());
        assert!(schema["properties"]["confidence"].is_object());
    }

    #[tokio::test]
    async fn execute_without_feature_or_db() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphHypothesisTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphHypothesisTool::new();

        let result = tool
            .execute(json!({"claim": "Rust is fast"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }
}
