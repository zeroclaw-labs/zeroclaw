//! MCP tool bridge: adapts zeroclaw's `Tool` registry into MCP-compatible
//! tool descriptors and dispatch.
//!
//! This module provides [`McpToolBridge`], which wraps either a static
//! `Vec<Box<dyn Tool>>` or a dynamic [`DynamicRegistry`] and exposes listing
//! (as [`McpToolDescriptor`]) and name-based invocation.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::dynamic_registry::DynamicRegistry;
use crate::tools::{Tool, ToolResult};

/// MCP-compatible tool descriptor containing the fields required by the
/// MCP `tools/list` response: name, description, and input schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Internal storage for the tool source.
enum ToolSource {
    /// Static tool set built at startup (original behavior).
    Static {
        tools: HashMap<String, Box<dyn Tool>>,
        order: Vec<String>,
    },
    /// Dynamic registry providing snapshot-based tool access, plus optional
    /// meta-tools that live outside the registry (to break circular deps).
    Dynamic {
        registry: Arc<DynamicRegistry>,
        meta_tools: Vec<Arc<dyn Tool>>,
    },
}

/// Adapter that indexes zeroclaw tools by name and exposes MCP-oriented
/// listing and dispatch operations.
///
/// Supports two modes:
/// - **Static** (`new`): tools are fixed at construction time.
/// - **Dynamic** (`from_registry`): tools are read from a [`DynamicRegistry`]
///   snapshot on each call, reflecting runtime additions/removals.
pub struct McpToolBridge {
    source: ToolSource,
}

impl McpToolBridge {
    /// Create a new bridge from a vec of boxed tools (static mode).
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
        Self {
            source: ToolSource::Static {
                tools: map,
                order,
            },
        }
    }

    /// Create a bridge backed by a [`DynamicRegistry`] (dynamic mode).
    ///
    /// Each call to `list_tools` and `call_tool` reads the current registry
    /// snapshot, so tools added or removed at runtime are immediately visible.
    ///
    /// `meta_tools` are always included alongside snapshot tools. This allows
    /// tools that reference the registry (like `ManageToolsTool`) to be
    /// visible in MCP without creating a circular dependency.
    pub fn from_registry(
        registry: Arc<DynamicRegistry>,
        meta_tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        Self {
            source: ToolSource::Dynamic {
                registry,
                meta_tools,
            },
        }
    }

    /// Current tool revision counter.
    ///
    /// Returns `0` for static bridges (never changes).
    /// For dynamic bridges, returns the registry's tool revision which
    /// increments on every tool add/remove/enable/disable.
    pub fn tool_revision(&self) -> u64 {
        match &self.source {
            ToolSource::Static { .. } => 0,
            ToolSource::Dynamic { registry, .. } => registry.tool_revision(),
        }
    }

    /// List all registered tools as MCP descriptors.
    ///
    /// In static mode, preserves insertion order. In dynamic mode, returns
    /// all enabled tools from the current registry snapshot.
    pub fn list_tools(&self) -> Vec<McpToolDescriptor> {
        match &self.source {
            ToolSource::Static { tools, order } => order
                .iter()
                .filter_map(|name| {
                    let tool = tools.get(name)?;
                    Some(McpToolDescriptor {
                        name: tool.name().to_string(),
                        description: tool.description().to_string(),
                        input_schema: tool.parameters_schema(),
                    })
                })
                .collect(),
            ToolSource::Dynamic {
                registry,
                meta_tools,
            } => {
                let snapshot = registry.snapshot();
                // Collect registry tool names to deduplicate meta-tools.
                let registry_names: std::collections::HashSet<&str> = snapshot
                    .all_tools
                    .iter()
                    .map(|t| t.name())
                    .collect();
                snapshot
                    .all_tools
                    .iter()
                    .map(|t| t.as_ref())
                    .chain(
                        meta_tools
                            .iter()
                            .filter(|t| !registry_names.contains(t.name()))
                            .map(|t| t.as_ref()),
                    )
                    .map(|tool| McpToolDescriptor {
                        name: tool.name().to_string(),
                        description: tool.description().to_string(),
                        input_schema: tool.parameters_schema(),
                    })
                    .collect()
            }
        }
    }

    /// Dispatch a tool call by name with the given JSON arguments.
    ///
    /// Returns an error if the tool name is not registered (or not enabled
    /// in dynamic mode).
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolResult> {
        match &self.source {
            ToolSource::Static { tools, .. } => {
                let tool = tools
                    .get(name)
                    .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", name))?;
                tool.execute(args).await
            }
            ToolSource::Dynamic {
                registry,
                meta_tools,
            } => {
                let snapshot = registry.snapshot();
                // Search registry tools first, then meta-tools.
                if let Some(tool) = snapshot.all_tools.iter().find(|t| t.name() == name) {
                    return tool.execute(args).await;
                }
                if let Some(tool) = meta_tools.iter().find(|t| t.name() == name) {
                    return tool.execute(args).await;
                }
                Err(anyhow::anyhow!("unknown tool: {}", name))
            }
        }
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

    // ── Dynamic registry bridge tests ─────────────────────────────────

    #[tokio::test]
    async fn dynamic_bridge_lists_tools_from_registry() {
        use crate::config::DynamicRegistryConfig;
        use crate::tools::dynamic_registry::{DynamicRegistry, DynamicToolDef};
        use chrono::Utc;

        let reg = Arc::new(DynamicRegistry::new_empty(DynamicRegistryConfig::default()));
        let bridge = McpToolBridge::from_registry(reg.clone(), vec![]);

        // Initially empty
        assert!(bridge.list_tools().is_empty());

        // Add a tool
        reg.add_tool(
            DynamicToolDef {
                id: "t1".into(),
                name: "dynamic_echo".into(),
                description: "Echoes".into(),
                kind: "shell_command".into(),
                config: json!({"command": "echo", "args": [{"Fixed": "hi"}]}),
                enabled: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                created_by: None,
            },
            None,
        )
        .unwrap();

        // Now visible
        let listed = bridge.list_tools();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "dynamic_echo");
    }

    #[tokio::test]
    async fn dynamic_bridge_call_tool_dispatches_to_snapshot() {
        use crate::config::DynamicRegistryConfig;
        use crate::tools::dynamic_registry::{DynamicRegistry, DynamicToolDef};
        use chrono::Utc;

        let reg = Arc::new(DynamicRegistry::new_empty(DynamicRegistryConfig::default()));
        let bridge = McpToolBridge::from_registry(reg.clone(), vec![]);

        reg.add_tool(
            DynamicToolDef {
                id: "t1".into(),
                name: "stub_tool".into(),
                description: "Stub".into(),
                kind: "shell_command".into(),
                config: json!({"command": "echo"}),
                enabled: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                created_by: None,
            },
            None,
        )
        .unwrap();

        // Call the dynamically added tool (StubDynamicTool returns "stub")
        let result = bridge.call_tool("stub_tool", json!({})).await.unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn dynamic_bridge_tool_revision_tracks_changes() {
        use crate::config::DynamicRegistryConfig;
        use crate::tools::dynamic_registry::{DynamicRegistry, DynamicToolDef};
        use chrono::Utc;

        let reg = Arc::new(DynamicRegistry::new_empty(DynamicRegistryConfig::default()));
        let bridge = McpToolBridge::from_registry(reg.clone(), vec![]);

        assert_eq!(bridge.tool_revision(), 0);

        reg.add_tool(
            DynamicToolDef {
                id: "t1".into(),
                name: "rev_tool".into(),
                description: "Track rev".into(),
                kind: "shell_command".into(),
                config: json!({"command": "echo"}),
                enabled: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                created_by: None,
            },
            None,
        )
        .unwrap();

        assert!(bridge.tool_revision() > 0);
    }

    #[test]
    fn static_bridge_tool_revision_is_always_zero() {
        let bridge = McpToolBridge::new(vec![]);
        assert_eq!(bridge.tool_revision(), 0);
    }

    #[tokio::test]
    async fn dynamic_bridge_includes_meta_tools_in_list_and_dispatch() {
        use crate::config::DynamicRegistryConfig;
        use crate::tools::dynamic_registry::DynamicRegistry;

        let reg = Arc::new(DynamicRegistry::new_empty(DynamicRegistryConfig::default()));

        // Create a meta-tool that lives outside the registry.
        struct MetaTool;
        #[async_trait]
        impl Tool for MetaTool {
            fn name(&self) -> &str {
                "manage_tools"
            }
            fn description(&self) -> &str {
                "Meta-tool for managing dynamic tools"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type": "object", "properties": {"action": {"type": "string"}}})
            }
            async fn execute(&self, _args: Value) -> Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: "meta-tool-ok".to_string(),
                    error: None,
                })
            }
        }

        let meta_tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MetaTool)];
        let bridge = McpToolBridge::from_registry(reg, meta_tools);

        // Meta-tool should appear in list.
        let listed = bridge.list_tools();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "manage_tools");

        // Meta-tool should be callable.
        let result = bridge
            .call_tool("manage_tools", json!({"action": "list"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "meta-tool-ok");
    }

    #[tokio::test]
    async fn dynamic_bridge_meta_tools_coexist_with_registry_tools() {
        use crate::config::DynamicRegistryConfig;
        use crate::tools::dynamic_registry::{DynamicRegistry, DynamicToolDef};
        use chrono::Utc;

        let reg = Arc::new(DynamicRegistry::new_empty(DynamicRegistryConfig::default()));

        // Add a dynamic tool to the registry.
        reg.add_tool(
            DynamicToolDef {
                id: "t1".into(),
                name: "echo_tool".into(),
                description: "Echoes".into(),
                kind: "shell_command".into(),
                config: json!({"command": "echo", "args": [{"Fixed": "hello"}]}),
                enabled: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                created_by: None,
            },
            None,
        )
        .unwrap();

        // Create a meta-tool.
        struct MetaTool;
        #[async_trait]
        impl Tool for MetaTool {
            fn name(&self) -> &str {
                "manage_tools"
            }
            fn description(&self) -> &str {
                "Meta"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }
            async fn execute(&self, _args: Value) -> Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: "meta".to_string(),
                    error: None,
                })
            }
        }

        let meta_tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MetaTool)];
        let bridge = McpToolBridge::from_registry(reg, meta_tools);

        // Both should appear in list.
        let listed = bridge.list_tools();
        assert_eq!(listed.len(), 2);
        let names: Vec<&str> = listed.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"echo_tool"));
        assert!(names.contains(&"manage_tools"));

        // Both should be callable.
        let r1 = bridge.call_tool("echo_tool", json!({})).await.unwrap();
        assert!(r1.success);
        let r2 = bridge
            .call_tool("manage_tools", json!({}))
            .await
            .unwrap();
        assert!(r2.success);
        assert_eq!(r2.output, "meta");
    }

    #[tokio::test]
    async fn dynamic_bridge_deduplicates_colliding_meta_tool_names() {
        use crate::config::DynamicRegistryConfig;
        use crate::tools::dynamic_registry::{DynamicRegistry, DynamicToolDef};
        use chrono::Utc;

        let reg = Arc::new(DynamicRegistry::new_empty(DynamicRegistryConfig::default()));

        // Add a dynamic tool with the same name as a meta-tool.
        reg.add_tool(
            DynamicToolDef {
                id: "t1".into(),
                name: "collider".into(),
                description: "Registry version".into(),
                kind: "shell_command".into(),
                config: json!({"command": "echo"}),
                enabled: true,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                created_by: None,
            },
            None,
        )
        .unwrap();

        // Create a meta-tool with the same name.
        struct CollidingMeta;
        #[async_trait]
        impl Tool for CollidingMeta {
            fn name(&self) -> &str {
                "collider"
            }
            fn description(&self) -> &str {
                "Meta version"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }
            async fn execute(&self, _args: Value) -> Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: "meta-collider".to_string(),
                    error: None,
                })
            }
        }

        let meta_tools: Vec<Arc<dyn Tool>> = vec![Arc::new(CollidingMeta)];
        let bridge = McpToolBridge::from_registry(reg, meta_tools);

        // Only one entry in the list (registry wins, meta-tool hidden).
        let listed = bridge.list_tools();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "collider");
        assert_eq!(listed[0].description, "Registry version");

        // Dispatch goes to registry tool (not meta-tool).
        let result = bridge.call_tool("collider", json!({})).await.unwrap();
        assert!(result.success);
        // StubDynamicTool returns "stub", not "meta-collider"
        assert_ne!(result.output, "meta-collider");
    }
}
