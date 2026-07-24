//! Bridge between WASM plugins and the Tool trait.

use crate::PluginCapability;
use crate::component::PluginLimits;
use crate::instance::PluginInstanceScope;
use crate::runtime;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
use zeroclaw_api::tool::{Tool, ToolResult};

/// A tool backed by a WASM plugin function.
pub struct WasmTool {
    name: String,
    description: String,
    parameters_schema: Value,
    wasm_path: PathBuf,
    scope: PluginInstanceScope,
    config: HashMap<String, String>,
    limits: PluginLimits,
}

impl Attributable for WasmTool {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::WasmPlugin)
    }

    fn alias(&self) -> &str {
        // `Role::Tool` writes this value to the canonical `tool` attribution
        // field. Keep it aligned with the callable export; package/capability/
        // binding identity remains on the host-issued scope and is emitted by
        // component logging under distinct plugin attributes.
        &self.name
    }
}

impl WasmTool {
    pub fn new(
        name: String,
        description: String,
        parameters_schema: Value,
        wasm_path: PathBuf,
        scope: PluginInstanceScope,
        config: HashMap<String, String>,
        limits: PluginLimits,
    ) -> anyhow::Result<Self> {
        scope.require_capability(PluginCapability::Tool)?;
        Ok(Self {
            name,
            description,
            parameters_schema,
            wasm_path,
            scope,
            config,
            limits,
        })
    }

    /// Create a `WasmTool` by loading its required metadata exports.
    ///
    /// Components that cannot be loaded, instantiated, or queried are rejected
    /// instead of being registered with synthetic metadata.
    pub fn from_wasm(
        wasm_path: PathBuf,
        scope: PluginInstanceScope,
        config: HashMap<String, String>,
        limits: PluginLimits,
    ) -> anyhow::Result<Self> {
        scope.require_capability(PluginCapability::Tool)?;
        let probe = {
            let wasm_path = wasm_path.clone();
            let scope = scope.clone();
            block_probe(async move {
                let mut plugin = runtime::create_plugin(&wasm_path, &scope, limits).await?;
                runtime::call_tool_metadata(&mut plugin).await
            })
        };
        let meta = probe?;

        Ok(Self {
            name: meta.name,
            description: meta.description,
            parameters_schema: meta.parameters_schema,
            wasm_path,
            scope,
            config,
            limits,
        })
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
        let mut plugin = runtime::create_plugin(&self.wasm_path, &self.scope, self.limits).await?;
        runtime::call_execute(&mut plugin, &args_json, &self.config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::attribution::{Attributable, Role, ToolKind};

    fn tool_scope() -> PluginInstanceScope {
        crate::instance::test_scope(PluginCapability::Tool, "redaction-primary", [])
    }

    #[test]
    fn tool_attribution_keeps_callable_and_instance_identities_distinct() {
        let schema = serde_json::json!({"type": "object", "properties": {}});
        let tool = WasmTool::new(
            "redact".to_string(),
            "does things".to_string(),
            schema.clone(),
            PathBuf::from("/tmp/plugin.wasm"),
            tool_scope(),
            HashMap::new(),
            crate::component::test_limits(1_000),
        )
        .expect("tool scope matches adapter");
        assert_eq!(tool.name(), "redact");
        assert_eq!(tool.description(), "does things");
        assert_eq!(tool.parameters_schema(), schema);
        assert_eq!(tool.role(), Role::Tool(ToolKind::WasmPlugin));
        assert_eq!(tool.alias(), "redact");
        assert_eq!(tool.scope.id().package(), "fixture");
        assert_eq!(tool.scope.id().capability(), PluginCapability::Tool);
        assert_eq!(tool.scope.id().binding(), "redaction-primary");
    }

    #[test]
    fn new_rejects_a_scope_for_another_capability() {
        let scope = crate::instance::test_scope(PluginCapability::Channel, "main", []);
        let result = WasmTool::new(
            "my_tool".to_string(),
            "does things".to_string(),
            serde_json::json!({}),
            PathBuf::from("/tmp/plugin.wasm"),
            scope,
            HashMap::new(),
            crate::component::test_limits(0),
        );

        assert!(result.is_err());
    }

    #[test]
    fn from_wasm_rejects_a_missing_component() {
        let result = WasmTool::from_wasm(
            PathBuf::from("/path/that/must/not/exist.wasm"),
            tool_scope(),
            HashMap::new(),
            crate::component::test_limits(0),
        );

        assert!(result.is_err());
    }
}
