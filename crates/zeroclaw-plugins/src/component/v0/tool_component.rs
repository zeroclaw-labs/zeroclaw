// Tool adapter: `ComponentTool` implements `zeroclaw_api::tool::Tool` backed by
// a WIT component-model plugin (the `tool-plugin` world in `wit/tool.wit`).
//
// Instance lifecycle: fresh per `execute` call — stateless, consistent with the
// existing `WasmTool` pattern.
//
// At construction the component bytes are compiled once and a `ToolPluginPre`
// is built via `Linker::instantiate_pre`.  Per `execute` call a fresh
// `Store<PluginStore>` is created and `pre.instantiate` does only the
// cheap per-instance wiring step.

use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

use super::bindings::tool::ToolPluginPre;
use super::plugin_store::{self, PluginStore};
use super::wrap_plugin;
use crate::component::engine::ComponentEngine;
use crate::error::PluginError;

tool_attribution!(ComponentTool, ToolKind::Plugin);

/// A tool backed by a WIT Component Model plugin (WASIP2 ABI).
pub struct ComponentTool {
    engine: Arc<ComponentEngine>,
    /// Pre-instantiated binding compiled from the component bytes once.
    /// `ToolPluginPre<PluginStore>` wraps an `InstancePre<PluginStore>`
    /// which is `Send + Sync`.
    pre: Arc<ToolPluginPre<PluginStore>>,
    name: String,
    description: String,
    parameters_schema: serde_json::Value,
    /// Canonical plugin name as self-reported by `plugin-info`. Source of truth.
    plugin_name: String,
    /// Plugin version string as self-reported by `plugin-info`. Source of truth.
    plugin_version: String,
    /// Fine-grained sandbox permissions applied to each execute store.
    permissions: Arc<Vec<crate::FineGrainedPermission>>,
}

impl ComponentTool {
    /// Load a `ComponentTool` by compiling the given WASM bytes, wiring host
    /// interfaces into the linker, building a pre-instantiated `ToolPluginPre`
    /// via `Linker::instantiate_pre`, and probing WIT metadata functions
    /// (`plugin-name`, `plugin-version`, `name`, `description`,
    /// `parameters-schema`) once with a throw-away store.
    ///
    /// `permissions` is stored and applied to every per-`execute` store so that
    /// filesystem, TCP, UDP, and HTTP access are restricted to the declared
    /// `fine_grained_permissions` list.
    pub async fn from_bytes(
        engine: Arc<ComponentEngine>,
        bytes: &[u8],
        permissions: Vec<crate::FineGrainedPermission>,
    ) -> anyhow::Result<Self> {
        let component = engine.compile(bytes)?;
        let mut linker = wasmtime::component::Linker::<PluginStore>::new(engine.engine());
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(PluginError::from)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
            .map_err(PluginError::from)?;
        plugin_store::add_to_linker_tool(&mut linker)?;

        // Build the InstancePre once; only cheap per-instance wiring happens
        // in execute().
        let instance_pre = linker
            .instantiate_pre(&component)
            .map_err(PluginError::from)?;
        let pre = Arc::new(ToolPluginPre::new(instance_pre).map_err(PluginError::from)?);

        // Probe metadata with a throw-away store.
        let mut store = wasmtime::Store::new(engine.engine(), PluginStore::default());
        let bindings = pre.instantiate(&mut store).map_err(PluginError::from)?;

        // Phase 2: read plugin-info exports — canonical source of truth.
        let plugin_info = bindings.zeroclaw_plugin_plugin_info();
        let plugin_name = plugin_info
            .call_plugin_name(&mut store)
            .await
            .map_err(PluginError::from)?;
        let plugin_version = plugin_info
            .call_plugin_version(&mut store)
            .await
            .map_err(PluginError::from)?;

        let exports = bindings.zeroclaw_plugin_tool();
        let name = exports
            .call_name(&mut store)
            .await
            .map_err(PluginError::from)?;
        let description = exports
            .call_description(&mut store)
            .await
            .map_err(PluginError::from)?;
        let schema_str = exports
            .call_parameters_schema(&mut store)
            .await
            .map_err(PluginError::from)?;
        let parameters_schema: serde_json::Value = serde_json::from_str(&schema_str)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        Ok(Self {
            engine,
            pre,
            name,
            description,
            parameters_schema,
            plugin_name,
            plugin_version,
            permissions: Arc::new(permissions),
        })
    }
}

#[async_trait]
impl Tool for ComponentTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters_schema.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let engine = Arc::clone(&self.engine);
        let pre = Arc::clone(&self.pre);
        let args_str = args.to_string();
        let plugin_name = self.plugin_name.clone();
        let plugin_version = self.plugin_version.clone();

        let permissions = Arc::clone(&self.permissions);
        wrap_plugin::wrap_plugin_call(&plugin_name, &plugin_version, "execute", async move {
            let host = PluginStore::with_permissions(&permissions).await?;
            let mut store = wasmtime::Store::new(engine.engine(), host);
            let bindings = pre.instantiate(&mut store).map_err(PluginError::from)?;
            let exports = bindings.zeroclaw_plugin_tool();

            let wit_result = exports
                .call_execute(&mut store, &args_str)
                .await
                .map_err(PluginError::from)?;

            match wit_result {
                Ok(wit_tool_result) => Ok(ToolResult {
                    success: wit_tool_result.success,
                    output: wit_tool_result.output,
                    error: wit_tool_result.error,
                }),
                Err(err_msg) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(err_msg),
                }),
            }
        })
        .await
    }
}
