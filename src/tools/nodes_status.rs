//! Tool for getting node status information

use async_trait::async_trait;
use crate::nodes::NodeServer;
use crate::tools::traits::{Tool, ToolResult};
use std::sync::Arc;

/// Tool to get node status information
pub struct NodesStatusTool {
    server: Arc<NodeServer>,
}

impl NodesStatusTool {
    /// Create a new nodes status tool
    pub fn new(server: Arc<NodeServer>) -> Self {
        Self { server }
    }
}

#[async_trait]
impl Tool for NodesStatusTool {
    fn name(&self) -> &str {
        "nodes_status"
    }

    fn description(&self) -> &str {
        "Get detailed status information for a specific node"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "Node ID to query"
                }
            },
            "required": ["node_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let node_id = args["node_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("node_id is required"))?;

        let node_info = self.server.get_node_status(node_id);

        match node_info {
            Some(info) => {
                let output = serde_json::to_string_pretty(&info)?;

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Node '{}' not found", node_id)),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NodesConfig;

    #[test]
    fn test_nodes_status_tool_name() {
        let config = NodesConfig::default();
        let server = Arc::new(NodeServer::new(config));
        let tool = NodesStatusTool::new(server);

        assert_eq!(tool.name(), "nodes_status");
    }

    #[test]
    fn test_nodes_status_tool_description() {
        let config = NodesConfig::default();
        let server = Arc::new(NodeServer::new(config));
        let tool = NodesStatusTool::new(server);

        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_nodes_status_tool_schema() {
        let config = NodesConfig::default();
        let server = Arc::new(NodeServer::new(config));
        let tool = NodesStatusTool::new(server);

        let schema = tool.parameters_schema();
        assert!(schema.is_object());
        assert!(schema["properties"]["node_id"].is_object());
    }
}