//! Bridge between WASM plugins and the Tool trait.

use crate::PluginPermission;
use crate::runtime;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

tool_attribution!(WasmTool, ToolKind::Plugin);

/// A tool backed by a WASM plugin function.
pub struct WasmTool {
    name: String,
    description: String,
    parameters_schema: Value,
    wasm_path: PathBuf,
    permissions: Vec<PluginPermission>,
    config: HashMap<String, String>,
}

impl WasmTool {
    pub fn new(
        name: String,
        description: String,
        parameters_schema: Value,
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
        config: HashMap<String, String>,
    ) -> Self {
        Self {
            name,
            description,
            parameters_schema,
            wasm_path,
            permissions,
            config,
        }
    }

    /// Create a WasmTool by loading metadata from the plugin's `tool` export.
    /// Falls back to manifest-supplied values if the export is missing.
    pub fn from_wasm(
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
        fallback_name: String,
        fallback_description: String,
        config: HashMap<String, String>,
    ) -> Self {
        let probe = {
            let wasm_path = wasm_path.clone();
            let permissions = permissions.clone();
            block_probe(async move {
                let mut plugin = runtime::create_plugin(&wasm_path, &permissions).await?;
                runtime::call_tool_metadata(&mut plugin).await
            })
        };
        let (name, description, schema) = match probe {
            Ok(meta) => (meta.name, meta.description, meta.parameters_schema),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "failed to load WASM plugin at {} for metadata: {e}",
                        wasm_path.display()
                    )
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
            config,
        }
    }
}

/// Run a one-shot async plugin probe to completion from a synchronous context.
/// A scratch current-thread runtime on a dedicated thread keeps this safe to
/// call whether or not an outer tokio runtime is active.
fn block_probe<F, T>(fut: F) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(fut)
            })
            .join()
            .map_err(|_| anyhow::Error::msg("plugin probe thread panicked"))?
    })
}

/// The JSON Schema returned when a plugin lacks a tool metadata export or fails
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
        let args_json = serde_json::to_vec(&args)?;
        let mut plugin = runtime::create_plugin(&self.wasm_path, &self.permissions).await?;
        runtime::call_execute(&mut plugin, &args_json, &self.config, &self.permissions).await
    }
}
