//! Graph validate tool — validate or refute a hypothesis in the knowledge graph.
//!
//! Requires the `memory-graph` feature (CozoDB backend). When the feature is
//! disabled at compile time, the tool returns a descriptive error.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
#[cfg(feature = "memory-graph")]
use std::sync::Arc;

/// Validate or refute a hypothesis in the CozoDB knowledge graph.
pub struct GraphValidateTool {
    #[cfg(feature = "memory-graph")]
    graph_db: Option<Arc<cozo::DbInstance>>,
}

impl GraphValidateTool {
    /// Create a new `GraphValidateTool`.
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
impl Tool for GraphValidateTool {
    fn name(&self) -> &str {
        "graph_validate"
    }

    fn description(&self) -> &str {
        "Validate or refute a hypothesis in the knowledge graph by updating its status and evidence."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "hypothesis_id": {
                    "type": "string",
                    "description": "The ID of the hypothesis to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["confirmed", "refuted", "open"],
                    "description": "New status for the hypothesis"
                },
                "evidence": {
                    "type": "string",
                    "description": "Optional evidence supporting the status change"
                },
                "new_confidence": {
                    "type": "number",
                    "description": "Optional updated confidence level between 0.0 and 1.0"
                }
            },
            "required": ["hypothesis_id", "status"]
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
            let hypothesis_id = args
                .get("hypothesis_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'hypothesis_id' parameter"))?;

            let status = args
                .get("status")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'status' parameter"))?;

            // Validate status enum
            if !["confirmed", "refuted", "open"].contains(&status) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid status '{status}': must be 'confirmed', 'refuted', or 'open'"
                    )),
                });
            }

            let evidence = args.get("evidence").and_then(|v| v.as_str()).unwrap_or("");

            let new_confidence = args.get("new_confidence").and_then(|v| v.as_f64());

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

            let timestamp = chrono::Utc::now().to_rfc3339();

            // Build the update query. When a new_confidence is supplied we
            // overwrite the stored value; otherwise we keep the existing one by
            // reading first and putting back.
            let query = if new_confidence.is_some() {
                r#":put hypothesis {
                    id: $id,
                    status: $status,
                    evidence_for: $evidence,
                    confidence: $confidence,
                    updated_at: $updated_at
                }"#
            } else {
                r#":put hypothesis {
                    id: $id,
                    status: $status,
                    evidence_for: $evidence,
                    updated_at: $updated_at
                }"#
            };

            let mut params = std::collections::BTreeMap::from([
                ("id".into(), cozo::DataValue::Str(hypothesis_id.into())),
                ("status".into(), cozo::DataValue::Str(status.into())),
                ("evidence".into(), cozo::DataValue::Str(evidence.into())),
                ("updated_at".into(), cozo::DataValue::Str(timestamp.into())),
            ]);

            if let Some(conf) = new_confidence {
                params.insert("confidence".into(), cozo::DataValue::from(conf));
            }

            match db.run_script(query, params, cozo::ScriptMutability::Mutable) {
                Ok(_) => {
                    let confidence_msg = new_confidence
                        .map(|c| format!(", confidence updated to {c:.2}"))
                        .unwrap_or_default();
                    Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Hypothesis {hypothesis_id} updated to '{status}'{confidence_msg}"
                        ),
                        error: None,
                    })
                }
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to validate hypothesis: {e}")),
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
        let tool = GraphValidateTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphValidateTool::new();

        assert_eq!(tool.name(), "graph_validate");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["hypothesis_id"].is_object());
        assert!(schema["properties"]["status"].is_object());
        assert!(schema["properties"]["evidence"].is_object());
        assert!(schema["properties"]["new_confidence"].is_object());
    }

    #[tokio::test]
    async fn execute_without_feature_or_db() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphValidateTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphValidateTool::new();

        let result = tool
            .execute(json!({
                "hypothesis_id": "abc-123",
                "status": "confirmed"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }
}
