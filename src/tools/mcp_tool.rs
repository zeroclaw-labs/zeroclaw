//! Wraps a discovered MCP tool as a zeroclaw [`Tool`] so it is dispatched
//! through the existing tool registry and agent loop without modification.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::tools::mcp_client::McpRegistry;
use crate::tools::mcp_protocol::McpToolDef;
use crate::tools::traits::{Tool, ToolResult};

/// A zeroclaw [`Tool`] backed by an MCP server tool.
///
/// The `prefixed_name` (e.g. `filesystem__read_file`) is what the agent loop
/// sees. The registry knows how to route it to the correct server.
pub struct McpToolWrapper {
    /// Prefixed name: `<server_name>__<tool_name>`.
    prefixed_name: String,
    /// Description extracted from the MCP tool definition. Stored as an owned
    /// String so that `description()` can return `&str` with self's lifetime.
    description: String,
    /// JSON schema for the tool's input parameters.
    input_schema: serde_json::Value,
    /// Shared registry — used to dispatch actual tool calls.
    registry: Arc<McpRegistry>,
}

impl McpToolWrapper {
    pub fn new(
        prefixed_name: String,
        def: McpToolDef,
        registry: Arc<McpRegistry>,
        server_env: &HashMap<String, String>,
    ) -> Self {
        let description = def.description.unwrap_or_else(|| "MCP tool".to_string());
        let input_schema = Self::strip_auto_injected_params(def.input_schema, server_env);
        Self {
            prefixed_name,
            description,
            input_schema,
            registry,
        }
    }

    fn strip_auto_injected_params(
        mut schema: serde_json::Value,
        server_env: &HashMap<String, String>,
    ) -> serde_json::Value {
        if server_env.is_empty() {
            return schema;
        }

        let env_lower: std::collections::HashSet<String> =
            server_env.keys().map(|k| k.to_ascii_lowercase()).collect();

        if let Some(obj) = schema.as_object_mut() {
            if let Some(serde_json::Value::Object(properties)) = obj.get_mut("properties") {
                let keys_to_remove: Vec<String> = properties
                    .keys()
                    .filter(|key| {
                        let key_lc = key.to_ascii_lowercase();
                        crate::tools::mcp_client::McpServer::is_credential_like_key(&key_lc)
                            && env_lower.contains(&key_lc)
                    })
                    .cloned()
                    .collect();

                for key in &keys_to_remove {
                    properties.remove(key);
                }

                if let Some(serde_json::Value::Array(required)) = obj.get_mut("required") {
                    required.retain(|value| {
                        value
                            .as_str()
                            .map(|required_key| {
                                !keys_to_remove.iter().any(|removed_key| {
                                    removed_key.eq_ignore_ascii_case(required_key)
                                })
                            })
                            .unwrap_or(true)
                    });
                }
            }
        }

        schema
    }
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        match self.registry.call_tool(&self.prefixed_name, args).await {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}
