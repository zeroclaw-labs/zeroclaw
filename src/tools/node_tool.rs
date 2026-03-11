use super::traits::{Tool, ToolResult};
use crate::nodes::NodeClient;
use crate::security::SecurityPolicy;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool that lets agents invoke actions on remote nodes in the cluster.
pub struct NodeTool {
    client: Arc<NodeClient>,
    security: Arc<SecurityPolicy>,
}

impl NodeTool {
    pub fn new(client: Arc<NodeClient>, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}

#[async_trait]
impl Tool for NodeTool {
    fn name(&self) -> &str {
        "node_invoke"
    }

    fn description(&self) -> &str {
        "Invoke an action on a remote node in the multi-machine cluster. \
         Requires a valid node_id, action name, and optional JSON payload."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "Target node identifier"
                },
                "action": {
                    "type": "string",
                    "description": "Action to invoke on the remote node (e.g. 'shell', 'health')"
                },
                "payload": {
                    "type": "object",
                    "description": "Optional JSON payload for the action",
                    "default": {}
                }
            },
            "required": ["node_id", "action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if self.security.autonomy == crate::security::AutonomyLevel::ReadOnly {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Node invocation blocked by security policy (autonomy=read_only)".into(),
                ),
            });
        }

        let node_id = args
            .get("node_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter 'node_id'"))?;

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter 'action'"))?;

        let payload = args
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let config = self.client.registry().config();
        if !config.allowed_node_ids.is_empty()
            && !config
                .allowed_node_ids
                .iter()
                .any(|id| id == "*" || id == node_id)
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Node '{node_id}' is not in the allowed list")),
            });
        }

        match self.client.invoke(node_id, action, payload).await {
            Ok(result) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&result).unwrap_or_default(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Node invocation failed: {e}")),
            }),
        }
    }
}
