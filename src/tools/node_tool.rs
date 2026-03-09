//! Wraps a node capability as a zeroclaw [`Tool`] so it can be dispatched
//! through the existing tool registry and agent loop.
//!
//! Tool names are prefixed with the node ID: `node:<node_id>:<capability_name>`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::time::Duration;

use crate::gateway::nodes::{NodeInvocation, NodeRegistry};
use crate::tools::traits::{Tool, ToolResult};

/// Default timeout for node invocations (30 seconds).
const NODE_INVOKE_TIMEOUT_SECS: u64 = 30;

/// A zeroclaw [`Tool`] backed by a node capability.
///
/// The `prefixed_name` (e.g. `node:phone-1:camera.snap`) is what the agent
/// loop sees. Invocations are routed to the connected node via WebSocket.
pub struct NodeTool {
    /// Prefixed name: `node:<node_id>:<capability_name>`.
    prefixed_name: String,
    /// The node ID this tool belongs to.
    node_id: String,
    /// The original capability name.
    capability_name: String,
    /// Human-readable description.
    description: String,
    /// JSON schema for parameters.
    parameters: serde_json::Value,
    /// Node registry for routing invocations.
    registry: Arc<NodeRegistry>,
}

impl NodeTool {
    /// Create a new node tool wrapper.
    pub fn new(
        node_id: String,
        capability_name: String,
        description: String,
        parameters: serde_json::Value,
        registry: Arc<NodeRegistry>,
    ) -> Self {
        let prefixed_name = format!("node:{node_id}:{capability_name}");
        Self {
            prefixed_name,
            node_id,
            capability_name,
            description,
            parameters,
            registry,
        }
    }

    /// Build the prefixed tool name for a node capability.
    pub fn tool_name(node_id: &str, capability_name: &str) -> String {
        format!("node:{node_id}:{capability_name}")
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
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Strip the `approved` field (same as MCP tools)
        let args = match args {
            serde_json::Value::Object(mut map) => {
                map.remove("approved");
                serde_json::Value::Object(map)
            }
            other => other,
        };

        let invoke_tx: tokio::sync::mpsc::Sender<NodeInvocation> =
            match self.registry.invoke_tx(&self.node_id) {
                Some(tx) => tx,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Node '{}' is not connected", self.node_id)),
                    });
                }
            };

        let call_id = uuid::Uuid::new_v4().to_string();
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        let invocation = NodeInvocation {
            call_id,
            capability: self.capability_name.clone(),
            args,
            response_tx,
        };

        if invoke_tx.send(invocation).await.is_err() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to send invocation to node '{}'",
                    self.node_id
                )),
            });
        }

        // Wait for response with timeout
        match tokio::time::timeout(Duration::from_secs(NODE_INVOKE_TIMEOUT_SECS), response_rx).await
        {
            Ok(Ok(result)) => Ok(ToolResult {
                success: result.success,
                output: result.output,
                error: result.error,
            }),
            Ok(Err(_)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Node '{}' dropped the invocation channel",
                    self.node_id
                )),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Node '{}' invocation timed out after {NODE_INVOKE_TIMEOUT_SECS}s",
                    self.node_id
                )),
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
<<<<<<< HEAD

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::nodes::{NodeCapability, NodeInfo, NodeRegistry};

    #[test]
    fn node_tool_name_format() {
        assert_eq!(
            NodeTool::tool_name("phone-1", "camera.snap"),
            "node:phone-1:camera.snap"
        );
    }

    #[test]
    fn node_tool_metadata() {
        let registry = Arc::new(NodeRegistry::new(10));
        let tool = NodeTool::new(
            "phone-1".to_string(),
            "camera.snap".to_string(),
            "Take a photo".to_string(),
            serde_json::json!({"type": "object", "properties": {"resolution": {"type": "string"}}}),
            registry,
        );

        assert_eq!(tool.name(), "node:phone-1:camera.snap");
        assert_eq!(tool.description(), "Take a photo");
        assert_eq!(tool.parameters_schema()["type"], "object");
    }

    #[tokio::test]
    async fn node_tool_execute_node_not_connected() {
        let registry = Arc::new(NodeRegistry::new(10));
        let tool = NodeTool::new(
            "missing-node".to_string(),
            "test".to_string(),
            "Test".to_string(),
            serde_json::json!({"type": "object", "properties": {}}),
            registry,
        );

        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not connected"));
    }

    #[tokio::test]
    async fn node_tool_execute_success() {
        let registry = Arc::new(NodeRegistry::new(10));
        let (invoke_tx, mut invoke_rx) = tokio::sync::mpsc::channel(32);

        registry.register(NodeInfo {
            node_id: "test-node".to_string(),
            capabilities: vec![NodeCapability {
                name: "echo".to_string(),
                description: "Echo back".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }],
            invoke_tx,
        });

        let tool = NodeTool::new(
            "test-node".to_string(),
            "echo".to_string(),
            "Echo back".to_string(),
            serde_json::json!({"type": "object", "properties": {}}),
            Arc::clone(&registry),
        );

        // Spawn a task that simulates the node responding
        tokio::spawn(async move {
            if let Some(invocation) = invoke_rx.recv().await {
                let _ = invocation
                    .response_tx
                    .send(crate::gateway::nodes::NodeInvocationResult {
                        success: true,
                        output: "echoed".to_string(),
                        error: None,
                    });
            }
        });

        let result = tool
            .execute(serde_json::json!({"msg": "hello"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "echoed");
        assert!(result.error.is_none());
    }

    #[test]
    fn node_tool_spec_generation() {
        let registry = Arc::new(NodeRegistry::new(10));
        let tool = NodeTool::new(
            "sensor-1".to_string(),
            "temp.read".to_string(),
            "Read temperature".to_string(),
            serde_json::json!({"type": "object", "properties": {"unit": {"type": "string"}}}),
            registry,
        );

        let spec = tool.spec();
        assert_eq!(spec.name, "node:sensor-1:temp.read");
        assert_eq!(spec.description, "Read temperature");
        assert!(spec.parameters["properties"]["unit"]["type"] == "string");
    }
}
=======
>>>>>>> 9dd885a8 (feat(nodes): implement functional multi-machine node system)
