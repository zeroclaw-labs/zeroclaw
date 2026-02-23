//! MCP tool bridge: adapts zeroclaw's `Tool` registry into MCP-compatible
//! tool descriptors and dispatch.
//!
//! This module provides [`McpToolBridge`], which wraps a `Vec<Box<dyn Tool>>`
//! and exposes listing (as [`McpToolDescriptor`]) and name-based invocation.

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{Tool, ToolResult};

/// MCP-compatible tool descriptor containing the fields required by the
/// MCP `tools/list` response: name, description, and input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Adapter that indexes zeroclaw tools by name and exposes MCP-oriented
/// listing and dispatch operations.
pub struct McpToolBridge {
    tools: HashMap<String, Box<dyn Tool>>,
    /// Insertion-order tool names so `list_tools` is deterministic.
    order: Vec<String>,
}

impl McpToolBridge {
    /// Create a new bridge from a vec of boxed tools.
    ///
    /// Tools are indexed by their `name()`. If duplicate names exist the last
    /// tool wins (consistent with registry override semantics).
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Self {
        let mut map = HashMap::with_capacity(tools.len());
        let mut order = Vec::with_capacity(tools.len());
        for tool in tools {
            let name = tool.name().to_string();
            if !map.contains_key(&name) {
                order.push(name.clone());
            }
            map.insert(name, tool);
        }
        Self { tools: map, order }
    }

    /// List all registered tools as MCP descriptors.
    ///
    /// The returned vec preserves the insertion order of tools passed to
    /// [`McpToolBridge::new`].
    pub fn list_tools(&self) -> Vec<McpToolDescriptor> {
        self.order
            .iter()
            .filter_map(|name| {
                let tool = self.tools.get(name)?;
                Some(McpToolDescriptor {
                    name: tool.name().to_string(),
                    description: tool.description().to_string(),
                    input_schema: tool.parameters_schema(),
                })
            })
            .collect()
    }

    /// Dispatch a tool call by name with the given JSON arguments.
    ///
    /// Returns an error if the tool name is not registered.
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolResult> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;
        tool.execute(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    /// Minimal deterministic tool for bridge testing.
    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy_tool"
        }

        fn description(&self) -> &str {
            "A dummy tool"
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(&self, args: Value) -> Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: args
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn list_tools_exports_name_description_and_schema() {
        let tools = vec![Box::new(DummyTool) as Box<dyn Tool>];
        let bridge = McpToolBridge::new(tools);
        let listed = bridge.list_tools();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "dummy_tool");
        assert_eq!(listed[0].description, "A dummy tool");
        assert_eq!(listed[0].input_schema["type"], "object");
        assert_eq!(
            listed[0].input_schema["properties"]["value"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn call_tool_routes_by_name() {
        let tools = vec![Box::new(DummyTool) as Box<dyn Tool>];
        let bridge = McpToolBridge::new(tools);
        let out = bridge
            .call_tool("dummy_tool", json!({"value": "ok"}))
            .await
            .expect("call should succeed");
        assert!(out.success);
        assert_eq!(out.output, "ok");
    }

    #[tokio::test]
    async fn call_tool_returns_error_for_unknown_tool() {
        let bridge = McpToolBridge::new(vec![]);
        let err = bridge.call_tool("nonexistent", json!({})).await;
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("unknown tool"),
            "error message should mention unknown tool, got: {}",
            msg
        );
    }

    #[tokio::test]
    async fn list_tools_preserves_insertion_order() {
        struct ToolA;
        struct ToolB;

        #[async_trait]
        impl Tool for ToolA {
            fn name(&self) -> &str {
                "alpha"
            }
            fn description(&self) -> &str {
                "first"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }
            async fn execute(&self, _args: Value) -> Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }

        #[async_trait]
        impl Tool for ToolB {
            fn name(&self) -> &str {
                "beta"
            }
            fn description(&self) -> &str {
                "second"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }
            async fn execute(&self, _args: Value) -> Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ToolA), Box::new(ToolB)];
        let bridge = McpToolBridge::new(tools);
        let listed = bridge.list_tools();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "alpha");
        assert_eq!(listed[1].name, "beta");
    }

    #[tokio::test]
    async fn empty_bridge_lists_no_tools() {
        let bridge = McpToolBridge::new(vec![]);
        assert!(bridge.list_tools().is_empty());
    }

    #[test]
    fn mcp_tool_descriptor_serde_roundtrip() {
        let desc = McpToolDescriptor {
            name: "test_tool".into(),
            description: "a test".into(),
            input_schema: json!({"type": "object"}),
        };
        let json_str = serde_json::to_string(&desc).unwrap();
        let parsed: McpToolDescriptor = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.name, "test_tool");
        assert_eq!(parsed.description, "a test");
    }
}
