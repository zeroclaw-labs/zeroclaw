//! Tool for getting node status information

use async_trait::async_trait;
use crate::nodes::NodeServer;
use crate::tools::traits::{Tool, ToolResult};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
        "Get status information for a specific node or overall server status if no node_id provided"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "Node ID to query (optional - if not provided, returns server status)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let node_id = args.get("node_id").and_then(|v| v.as_str());

        match node_id {
            Some(id) => {
                // Get specific node status
                let node_info = self.server.get_node_status(id);

                match node_info {
                    Some(info) => {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let last_seen_ago = now.saturating_sub(info.last_seen);
                        let status = if last_seen_ago < 60 {
                            "online"
                        } else if last_seen_ago < 180 {
                            "idle"
                        } else {
                            "offline"
                        };

                        let output = serde_json::json!({
                            "node": info,
                            "status": status,
                            "last_seen_seconds_ago": last_seen_ago
                        });

                        Ok(ToolResult {
                            success: true,
                            output: serde_json::to_string_pretty(&output)?,
                            error: None,
                        })
                    }
                    None => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Node '{}' not found. Use nodes_list to see connected nodes.", id)),
                    }),
                }
            }
            None => {
                // Get overall server status
                let nodes = self.server.list_nodes();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                let mut online = 0;
                let mut idle = 0;
                let mut offline = 0;

                for node in &nodes {
                    let last_seen_ago = now.saturating_sub(node.last_seen);
                    if last_seen_ago < 60 {
                        online += 1;
                    } else if last_seen_ago < 180 {
                        idle += 1;
                    } else {
                        offline += 1;
                    }
                }

                let output = serde_json::json!({
                    "server_status": "running",
                    "total_nodes": nodes.len(),
                    "online": online,
                    "idle": idle,
                    "offline": offline,
                    "nodes": nodes.iter().map(|n| {
                        let last_seen_ago = now.saturating_sub(n.last_seen);
                        let status = if last_seen_ago < 60 {
                            "online"
                        } else if last_seen_ago < 180 {
                            "idle"
                        } else {
                            "offline"
                        };
                        serde_json::json!({
                            "id": n.id,
                            "name": n.name,
                            "status": status,
                            "last_seen_seconds_ago": last_seen_ago
                        })
                    }).collect::<Vec<_>>()
                });

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&output)?,
                    error: None,
                })
            }
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