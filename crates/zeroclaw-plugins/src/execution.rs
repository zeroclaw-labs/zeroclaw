//! Execution facade for tool plugins.
//!
//! Two backends, one surface:
//! - built with `plugins-wasmtime`: calls run in-process through wasmtime.
//! - built without: calls are forwarded to the `zeroclaw-plugin-host` sidecar
//!   subprocess over stdio, so the main binary carries zero WASM-runtime weight.
//!
//! Callers (`WasmTool`, the runtime tool registry) only ever touch this module;
//! the backend choice is a compile-time detail.

use crate::PluginPermission;
use crate::subprocess::WireToolResult;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Resolved per-call execution limits applied to a plugin store. The host
/// builds this from `[plugins.limits]` config and hands it to `new_store`.
/// There is deliberately no `Default`: limits always come from the config
/// registry so no code path can construct an unsandboxed store by accident.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PluginLimits {
    pub call_fuel: u64,
    pub max_memory_bytes: usize,
    pub max_table_elements: usize,
    pub max_instances: usize,
}

/// Tool metadata read from a plugin's exported `tool` interface.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

/// Read the exported tool's metadata (name, description, parameters schema).
pub async fn tool_metadata(
    wasm_path: &Path,
    permissions: &[PluginPermission],
    limits: PluginLimits,
) -> Result<ToolMetadata> {
    #[cfg(feature = "plugins-wasmtime")]
    {
        let mut plugin = crate::runtime::create_plugin(wasm_path, permissions, limits).await?;
        let meta = crate::runtime::call_tool_metadata(&mut plugin).await?;
        Ok(ToolMetadata {
            name: meta.name,
            description: meta.description,
            parameters_schema: meta.parameters_schema,
        })
    }
    #[cfg(not(feature = "plugins-wasmtime"))]
    {
        let req = crate::subprocess::Request::ToolMetadata {
            wasm_path: wasm_path.to_path_buf(),
            permissions: permissions.to_vec(),
            limits,
        };
        let value = crate::subprocess::call(&req).await?;
        Ok(serde_json::from_value(value)?)
    }
}

/// Invoke the exported tool's `execute`, injecting the plugin's resolved config.
pub async fn tool_execute(
    wasm_path: &Path,
    permissions: &[PluginPermission],
    limits: PluginLimits,
    args: serde_json::Value,
    config: &HashMap<String, String>,
) -> Result<WireToolResult> {
    #[cfg(feature = "plugins-wasmtime")]
    {
        let args_json = serde_json::to_vec(&args)?;
        let mut plugin = crate::runtime::create_plugin(wasm_path, permissions, limits).await?;
        crate::runtime::call_execute(&mut plugin, &args_json, config, permissions).await
    }
    #[cfg(not(feature = "plugins-wasmtime"))]
    {
        let req = crate::subprocess::Request::ToolExecute {
            wasm_path: wasm_path.to_path_buf(),
            permissions: permissions.to_vec(),
            limits,
            args,
            config: config.clone(),
        };
        let value = crate::subprocess::call(&req).await?;
        Ok(serde_json::from_value(value)?)
    }
}
