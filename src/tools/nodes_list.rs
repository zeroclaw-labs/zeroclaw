//! Tool for listing all connected nodes

use crate::nodes::NodeServer;
use crate::tools::traits::{Tool, ToolResult};
use std::sync::Arc;

/// Tool to list all connected nodes
pub struct NodesListTool {
    server: Arc<NodeServer>,
}

impl NodesListTool {
    /// Create a new nodes list tool
    pub fn new(server: Arc<NodeServer>) -> Self {
        Self { server }
    }
}

impl Tool for NodesListTool {
    fn name(&self) -> &str {
        "nodes_list"
    }

    fn description(&self) -> &str {
        "List all connected nodes"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let nodes = self.server.list_nodes();

        if nodes.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No nodes connected".to_string(),
                error: None,
            });
        }

        let output = serde_json::to_string_pretty(&nodes)?;

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NodesConfig;

    #[test]
    fn test_nodes_list_tool_name() {
        let config = NodesConfig::default();
        let server = Arc::new(NodeServer::new(config));
        let tool = NodesListTool::new(server);

        assert_eq!(tool.name(), "nodes_list");
    }

    #[test]
    fn test_nodes_list_tool_description() {
        let config = NodesConfig::default();
        let server = Arc::new(NodeServer::new(config));
        let tool = NodesListTool::new(server);

        assert!(!tool.description().is_empty());
    }
}