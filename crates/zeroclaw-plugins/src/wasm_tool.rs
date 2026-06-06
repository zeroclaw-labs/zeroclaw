//! Bridge between WASM plugins and the Tool trait.

use crate::PluginPermission;
use crate::runtime;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

tool_attribution!(WasmTool, ToolKind::Plugin);

/// Wall-clock backstop for a single plugin call, in milliseconds. Set just
/// above the Extism epoch timeout ([`runtime::EXEC_TIMEOUT_MS`]) so Extism's
/// interrupt is the normal path for runaway wasm; this only covers the case
/// Extism cannot — a plugin wedged inside the host's blocking HTTP call after
/// its own client timeout. NOTE: `tokio::time::timeout` frees the async caller
/// but cannot kill the `spawn_blocking` thread; Extism's epoch timeout is what
/// actually interrupts a wasm-bound loop.
const WALL_CLOCK_TIMEOUT_MS: u64 = 185_000;

/// Compile-time invariant: the wall-clock backstop must outlast Extism's timeout.
const _: () = assert!(WALL_CLOCK_TIMEOUT_MS >= runtime::EXEC_TIMEOUT_MS);

/// A tool backed by a WASM plugin function.
pub struct WasmTool {
    name: String,
    description: String,
    parameters_schema: Value,
    wasm_path: PathBuf,
    permissions: Vec<PluginPermission>,
    allowed_hosts: Option<Vec<String>>,
    env_allowlist: Option<Vec<String>>,
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
            allowed_hosts: None,
            env_allowlist: None,
        }
    }

    /// Create a WasmTool by loading metadata from the plugin's `tool_metadata` export.
    /// Falls back to manifest-supplied values if the export is missing.
    pub fn from_wasm(
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
        fallback_name: String,
        fallback_description: String,
        allowed_hosts: Option<Vec<String>>,
        env_allowlist: Option<Vec<String>>,
    ) -> Self {
        // Try to load metadata from the WASM module itself.
        let (name, description, schema) = match runtime::create_plugin_with(
            &wasm_path,
            &permissions,
            allowed_hosts.as_deref(),
            env_allowlist.as_deref(),
        ) {
            Ok(mut plugin) => match runtime::call_tool_metadata(&mut plugin) {
                Ok(meta) => (meta.name, meta.description, meta.parameters_schema),
                Err(e) => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!(
                            "plugin at {} has no tool_metadata export ({e}), using fallback",
                            wasm_path.display()
                        )
                    );
                    (
                        fallback_name.clone(),
                        fallback_description.clone(),
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
            allowed_hosts,
            env_allowlist,
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
        let allowed_hosts = self.allowed_hosts.clone();
        let env_allowlist = self.env_allowlist.clone();
        let args_json = serde_json::to_vec(&args)?;

        // Extism Plugin is !Send, so we must create it inside spawn_blocking.
        let handle = tokio::task::spawn_blocking(move || {
            let mut plugin = runtime::create_plugin_with(
                &wasm_path,
                &permissions,
                allowed_hosts.as_deref(),
                env_allowlist.as_deref(),
            )?;
            runtime::call_execute(&mut plugin, &args_json)
        });

        // Wall-clock backstop. Extism's epoch timeout is the primary interruptor
        // for runaway wasm; this frees the async caller in the rare case the
        // blocking thread is wedged in a host call. The blocking thread is not
        // killed — it exits when call_execute returns (via Extism's own timeout).
        match tokio::time::timeout(
            std::time::Duration::from_millis(WALL_CLOCK_TIMEOUT_MS),
            handle,
        )
        .await
        {
            Ok(joined) => joined?,
            Err(_elapsed) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("plugin execution timed out".into()),
            }),
        }
    }
}
