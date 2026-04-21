//! Bridge between WASM plugins and the Tool trait.

use crate::PluginPermission;
use crate::runtime;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use zeroclaw_api::tool::{Tool, ToolResult};

/// A tool backed by a WASM plugin function.
pub struct WasmTool {
    name: String,
    description: String,
    parameters_schema: Value,
    wasm_path: PathBuf,
    permissions: Vec<PluginPermission>,
}

impl WasmTool {
    pub fn new(
        name: String,
        description: String,
        parameters_schema: Value,
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
    ) -> Self {
        Self {
            name,
            description,
            parameters_schema,
            wasm_path,
            permissions,
        }
    }

    /// Create a WasmTool by loading metadata from the plugin's `tool_metadata` export.
    /// Falls back to manifest-supplied values if the export is missing.
    pub fn from_wasm(
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
        fallback_name: String,
        fallback_description: String,
    ) -> Self {
        // Try to load metadata from the WASM module itself.
        let (name, description, schema) = match runtime::create_plugin(&wasm_path, &permissions) {
            Ok(mut plugin) => match runtime::call_tool_metadata(&mut plugin) {
                Ok(meta) => (meta.name, meta.description, meta.parameters_schema),
                Err(e) => {
                    tracing::debug!(
                        "plugin at {} has no tool_metadata export ({e}), using fallback",
                        wasm_path.display()
                    );
                    (
                        fallback_name.clone(),
                        fallback_description.clone(),
                        default_schema(),
                    )
                }
            },
            Err(e) => {
                tracing::warn!(
                    "failed to load WASM plugin at {} for metadata: {e}",
                    wasm_path.display()
                );
                (
                    fallback_name.clone(),
                    fallback_description.clone(),
                    default_schema(),
                )
            }
        };

        Self {
            name,
            description,
            parameters_schema: schema,
            wasm_path,
            permissions,
        }
    }
}

/// The JSON Schema returned when a plugin lacks a `tool_metadata` export or fails
/// to load at discovery time. Single source of truth so the fallback shape stays
/// consistent across code paths.
fn default_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "input": {
                "type": "string",
                "description": "Input for the plugin"
            }
        },
        "required": ["input"]
    })
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
        let wasm_path = self.wasm_path.clone();
        let permissions = self.permissions.clone();
        let args_json = serde_json::to_vec(&args)?;

        // Extism Plugin is !Send, so we must create it inside spawn_blocking.
        tokio::task::spawn_blocking(move || {
            let mut plugin = runtime::create_plugin(&wasm_path, &permissions)?;
            runtime::call_execute(&mut plugin, &args_json)
        })
        .await?
    }
}
