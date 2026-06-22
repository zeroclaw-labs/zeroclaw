//! Value types for the WIT tool runtime: prepared component, request, and the
//! per-execution result. Ported from `ironclaw_wasm::types`, adapted to
//! zeroclaw's `tool` WIT interface (which carries an explicit `success` flag and
//! a tool-supplied `name`).

use crate::usage::ResourceUsage;
use crate::wit_config::WitToolLimits;

/// A compiled WIT tool component plus the metadata read from its WIT exports.
pub struct PreparedWitTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) schema: serde_json::Value,
    pub(crate) component: wasmtime::component::Component,
    pub(crate) limits: WitToolLimits,
}

impl PreparedWitTool {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn schema(&self) -> &serde_json::Value {
        &self.schema
    }

    pub fn limits(&self) -> &WitToolLimits {
        &self.limits
    }
}

impl std::fmt::Debug for PreparedWitTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedWitTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("schema", &self.schema)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// Arguments passed to a WIT tool's `execute` export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitToolRequest {
    pub args_json: String,
}

impl WitToolRequest {
    pub fn new(args_json: impl Into<String>) -> Self {
        Self {
            args_json: args_json.into(),
        }
    }
}

/// Severity captured from the guest's `logging.log-record` calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmLogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// One guest-emitted log message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmLogRecord {
    pub level: WasmLogLevel,
    pub message: String,
}

/// The result of one WIT tool execution.
///
/// Mirrors zeroclaw's `tool.tool-result` (`success`/`output`/`error`) so it maps
/// 1:1 onto `zeroclaw_api::tool::ToolResult`, plus accounting and captured logs.
#[derive(Debug, Clone, PartialEq)]
pub struct WitToolExecution {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub usage: ResourceUsage,
    pub logs: Vec<WasmLogRecord>,
}
