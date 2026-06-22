//! Bridge from a WIT component plugin to zeroclaw's `Tool` trait.
//!
//! Replaces the Extism `wasm_tool.rs`. A single process-wide [`WitToolRuntime`]
//! (one wasmtime engine + one epoch ticker) is shared across all plugins; each
//! tool holds its compiled component (`PreparedWitTool`) and the host services
//! it was granted. `execute` runs the sync wasmtime call on the blocking pool —
//! the engine/component/host are all `Send + Sync`, so unlike Extism nothing is
//! `!Send` and only the per-call execution touches the blocking thread.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::Value;
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

use crate::PluginPermission;
use crate::wit_config::WitToolRuntimeConfig;
use crate::wit_host::WitToolHost;
use crate::wit_runtime::WitToolRuntime;
use crate::wit_types::{PreparedWitTool, WitToolRequest};

tool_attribution!(WitTool, ToolKind::Plugin);

/// The process-wide shared component runtime. `None` if the engine could not be
/// created (an unsupported build with no compiler backend, say) — every plugin
/// then fails closed at execution.
fn shared_runtime() -> Option<Arc<WitToolRuntime>> {
    static RUNTIME: OnceLock<Option<Arc<WitToolRuntime>>> = OnceLock::new();
    RUNTIME
        .get_or_init(
            || match WitToolRuntime::new(WitToolRuntimeConfig::default()) {
                Ok(runtime) => Some(Arc::new(runtime)),
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!("failed to create WIT plugin runtime: {error}")
                    );
                    None
                }
            },
        )
        .clone()
}

/// A tool backed by a WIT component plugin.
pub struct WitTool {
    name: String,
    description: String,
    parameters_schema: Value,
    #[allow(dead_code)] // surfaced for diagnostics / future per-call gating
    permissions: Vec<PluginPermission>,
    runtime: Option<Arc<WitToolRuntime>>,
    prepared: Option<Arc<PreparedWitTool>>,
    host: WitToolHost,
}

impl WitTool {
    /// Load a tool from a component file, reading its metadata from the WIT
    /// exports. Falls back to the manifest-supplied name/description/schema if
    /// the component cannot be compiled. Host capabilities default to deny-all;
    /// the agent runtime grants the real services via [`WitTool::with_host`].
    pub fn from_wasm(
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
        fallback_name: String,
        fallback_description: String,
    ) -> Self {
        let runtime = shared_runtime();
        let prepared = runtime
            .as_ref()
            .and_then(|runtime| load_prepared(runtime, &wasm_path, &fallback_name));

        let (name, description, parameters_schema) = match &prepared {
            Some(prepared) => (
                prepared.name().to_string(),
                prepared.description().to_string(),
                prepared.schema().clone(),
            ),
            None => (fallback_name, fallback_description, default_schema()),
        };

        Self {
            name,
            description,
            parameters_schema,
            permissions,
            runtime,
            prepared,
            host: WitToolHost::deny_all(),
        }
    }

    /// Install the host services this plugin was granted (HTTP egress, workspace
    /// reader, secret/tool-invoke bridges, clock). Without this the tool runs
    /// fully sandboxed (every capability denied).
    pub fn with_host(mut self, host: WitToolHost) -> Self {
        self.host = host;
        self
    }
}

fn load_prepared(
    runtime: &Arc<WitToolRuntime>,
    wasm_path: &PathBuf,
    fallback_name: &str,
) -> Option<Arc<PreparedWitTool>> {
    let bytes = match std::fs::read(wasm_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "failed to read plugin component {}: {error}",
                    wasm_path.display()
                )
            );
            return None;
        }
    };
    match runtime.prepare(fallback_name, &bytes) {
        Ok(prepared) => Some(Arc::new(prepared)),
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "failed to load WIT plugin component {}: {error}",
                    wasm_path.display()
                )
            );
            None
        }
    }
}

/// Schema used when a component fails to load and only manifest metadata is left.
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
impl Tool for WitTool {
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
        let (Some(runtime), Some(prepared)) = (self.runtime.clone(), self.prepared.clone()) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("plugin '{}' failed to load", self.name)),
            });
        };
        let host = self.host.clone();
        let args_json = serde_json::to_string(&args)?;

        // wasmtime component calls are sync; run on the blocking pool. Everything
        // captured here is Send, so a fresh Store is created inside the closure.
        tokio::task::spawn_blocking(move || {
            let execution = runtime.execute(&prepared, host, WitToolRequest::new(args_json))?;
            Ok::<_, anyhow::Error>(ToolResult {
                success: execution.success,
                output: execution.output,
                error: execution.error,
            })
        })
        .await?
    }
}
