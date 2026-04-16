//! Bridge between WASM plugins and the Tool trait.

use async_trait::async_trait;
use serde_json::Value;
use zeroclaw_api::tool::{Tool, ToolResult};

/// A tool backed by a WASM plugin function.
pub struct WasmTool {
    name: String,
    description: String,
    plugin_name: String,
    function_name: String,
    parameters_schema: Value,
}

impl WasmTool {
    pub fn new(
        name: String,
        description: String,
        plugin_name: String,
        function_name: String,
        parameters_schema: Value,
    ) -> Self {
        Self {
            name,
            description,
            plugin_name,
            function_name,
            parameters_schema,
        }
    }
}

#[async_trait]
impl Tool for WasmTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters_schema.clone()
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        // TODO: Call into Extism plugin runtime
        // For now, return a placeholder indicating the plugin system is available
        // but not yet wired to actual WASM execution.
        Ok(ToolResult {
            success: false,
            output: format!(
                "[plugin:{}/{}] WASM execution not yet connected. Args: {}",
                self.plugin_name,
                self.function_name,
                serde_json::to_string(&args).unwrap_or_default()
            ),
            error: Some("WASM execution bridge not yet implemented".into()),
        })
    }
}
