//! Tool for executing commands on remote nodes

use async_trait::async_trait;
use crate::nodes::NodeServer;
use crate::tools::traits::{Tool, ToolResult};
use std::sync::Arc;

/// Tool to execute commands on remote nodes
pub struct NodesRunTool {
    server: Arc<NodeServer>,
}

impl NodesRunTool {
    /// Create a new nodes run tool
    pub fn new(server: Arc<NodeServer>) -> Self {
        Self { server }
    }
}

#[async_trait]
impl Tool for NodesRunTool {
    fn name(&self) -> &str {
        "nodes_run"
    }

    fn description(&self) -> &str {
        "Execute a command on a remote node"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "Target node ID"
                },
                "command": {
                    "type": "string",
                    "description": "Command to execute"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 60)"
                }
            },
            "required": ["node_id", "command"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let node_id = args["node_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("node_id is required"))?;

        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("command is required"))?;

        let timeout_secs = args["timeout_secs"].as_u64().map(|t| t as u32);

        let result = self
            .server
            .run_command(node_id, command, timeout_secs)
            .await;

        match result {
            Ok(response) => {
                let output = match response {
                    crate::nodes::NodeResponse::ExecResult {
                        success,
                        stdout,
                        stderr,
                        exit_code,
                    } => {
                        format!(
                            "Exit code: {}\nSuccess: {}\n\nSTDOUT:\n{}\n\nSTDERR:\n{}",
                            exit_code, success, stdout, stderr
                        )
                    }
                    _ => serde_json::to_string_pretty(&response)?,
                };

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NodesConfig;

    #[test]
    fn test_nodes_run_tool_name() {
        let config = NodesConfig::default();
        let server = Arc::new(NodeServer::new(config));
        let tool = NodesRunTool::new(server);

        assert_eq!(tool.name(), "nodes_run");
    }

    #[test]
    fn test_nodes_run_tool_description() {
        let config = NodesConfig::default();
        let server = Arc::new(NodeServer::new(config));
        let tool = NodesRunTool::new(server);

        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_nodes_run_tool_schema() {
        let config = NodesConfig::default();
        let server = Arc::new(NodeServer::new(config));
        let tool = NodesRunTool::new(server);

        let schema = tool.parameters_schema();
        assert!(schema.is_object());
        assert!(schema["properties"]["node_id"].is_object());
        assert!(schema["properties"]["command"].is_object());
    }
}