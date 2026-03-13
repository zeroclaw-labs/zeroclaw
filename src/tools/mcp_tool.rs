//! Wraps a discovered MCP tool as a zeroclaw [`Tool`] so it is dispatched
//! through the existing tool registry and agent loop without modification.

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
    pub fn new(prefixed_name: String, def: McpToolDef, registry: Arc<McpRegistry>) -> Self {
        let description = def.description.unwrap_or_else(|| "MCP tool".to_string());
        Self {
            prefixed_name,
            description,
            input_schema: def.input_schema,
            registry,
        }
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
