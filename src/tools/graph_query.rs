//! Graph query tool — execute Datalog queries on the knowledge graph.
//!
//! Requires the `memory-graph` feature (CozoDB backend). When the feature is
//! disabled at compile time, the tool returns a descriptive error.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
#[cfg(feature = "memory-graph")]
use std::sync::Arc;

/// Execute a Datalog query on the CozoDB knowledge graph.
pub struct GraphQueryTool {
    #[cfg(feature = "memory-graph")]
    graph_db: Option<Arc<cozo::DbInstance>>,
}

impl GraphQueryTool {
    /// Create a new `GraphQueryTool`.
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
impl Tool for GraphQueryTool {
    fn name(&self) -> &str {
        "graph_query"
    }

    fn description(&self) -> &str {
        "Execute a Datalog query on the knowledge graph. Use CozoDB Datalog syntax."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A CozoDB Datalog query string"
                },
                "params": {
                    "type": "object",
                    "description": "Optional parameter bindings for the query"
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
                error: Some("Graph memory requires `memory-graph` feature".into()),
            });
        }

        #[cfg(feature = "memory-graph")]
        {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;

            let params = args
                .get("params")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();

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

            let cozo_params: std::collections::BTreeMap<String, cozo::DataValue> = params
                .into_iter()
                .map(|(k, v)| {
                    let dv = match v {
                        serde_json::Value::String(s) => cozo::DataValue::Str(s.into()),
                        serde_json::Value::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                cozo::DataValue::from(i)
                            } else if let Some(f) = n.as_f64() {
                                cozo::DataValue::from(f)
                            } else {
                                cozo::DataValue::Str(n.to_string().into())
                            }
                        }
                        serde_json::Value::Bool(b) => cozo::DataValue::from(b),
                        other => cozo::DataValue::Str(other.to_string().into()),
                    };
                    (k, dv)
                })
                .collect();

            match db.run_script(query, cozo_params, cozo::ScriptMutability::Immutable) {
                Ok(result) => {
                    let output = serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|_| format!("{result:?}"));
                    Ok(ToolResult {
                        success: true,
                        output,
                        error: None,
                    })
                }
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Datalog query failed: {e}")),
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
        let tool = GraphQueryTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphQueryTool::new();

        assert_eq!(tool.name(), "graph_query");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["query"].is_object());
    }

    #[tokio::test]
    async fn execute_without_feature_or_db() {
        #[cfg(feature = "memory-graph")]
        let tool = GraphQueryTool::new(None);
        #[cfg(not(feature = "memory-graph"))]
        let tool = GraphQueryTool::new();

        let result = tool
            .execute(json!({"query": "?[] <- [[1, 2]]"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }
}
