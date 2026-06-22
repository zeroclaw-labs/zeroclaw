//! Bridge between WASM plugins and the Tool trait.

use crate::runtime;
use crate::{PluginManifest, PluginPermission};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

tool_attribution!(WasmTool, ToolKind::Plugin);

/// A tool backed by a WASM plugin function.
///
/// Carries the full `PluginManifest` (not just the permission bit-set) so
/// the host functions can enforce `http_allowed_hosts` / `env_read_vars`
/// allowlists at runtime — see issues #5918 and #5919.
pub struct WasmTool {
    name: String,
    description: String,
    parameters_schema: Value,
    wasm_path: PathBuf,
    manifest: PluginManifest,
}

impl WasmTool {
    /// Construct a `WasmTool` from a fully-built manifest. Used by the
    /// binary crate's synthetic advertisement path (`zeroclaw/src/tools/mod.rs`)
    /// and any external caller that already holds a manifest.
    pub fn new(
        manifest: PluginManifest,
        wasm_path: PathBuf,
        fallback_name: String,
        fallback_description: String,
    ) -> Self {
        Self {
            name: fallback_name,
            description: fallback_description,
            parameters_schema: default_schema(),
            wasm_path,
            manifest,
        }
    }

    /// Create a WasmTool by loading metadata from the plugin's `tool_metadata` export.
    /// Falls back to manifest-supplied values if the export is missing.
    pub fn from_wasm(manifest: &PluginManifest, wasm_path: PathBuf) -> Self {
        // Try to load metadata from the WASM module itself.
        let (name, description, schema) =
            match runtime::create_plugin_with_manifest(&wasm_path, manifest) {
                Ok(mut plugin) => match runtime::call_tool_metadata(&mut plugin) {
                    Ok(meta) => (meta.name, meta.description, meta.parameters_schema),
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!(
                                "plugin at {} has no tool_metadata export ({e}), using fallback",
                                wasm_path.display()
                            )
                        );
                        (
                            manifest.name.clone(),
                            manifest.description.clone().unwrap_or_default(),
                            default_schema(),
                        )
                    }
                },
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
                        manifest.name.clone(),
                        manifest.description.clone().unwrap_or_default(),
                        default_schema(),
                    )
                }
            };

        Self {
            name,
            description,
            parameters_schema: schema,
            wasm_path,
            manifest: manifest.clone(),
        }
    }

    /// Borrow the manifest's permissions. Used by the runtime caller that
    /// needs to surface them in a UI / debug surface without cloning.
    #[allow(dead_code)]
    pub fn permissions(&self) -> &[PluginPermission] {
        &self.manifest.permissions
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
        let manifest = self.manifest.clone();
        let args_json = serde_json::to_vec(&args)?;

        // Extism Plugin is !Send, so we must create it inside spawn_blocking.
        // The full manifest is cloned into the closure so the host functions
        // can enforce http_allowed_hosts / env_read_vars at call time.
        tokio::task::spawn_blocking(move || {
            let mut plugin = runtime::create_plugin_with_manifest(&wasm_path, &manifest)?;
            runtime::call_execute(&mut plugin, &args_json)
        })
        .await?
    }
}
