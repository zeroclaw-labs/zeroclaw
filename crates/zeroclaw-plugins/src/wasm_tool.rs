//! Bridge between WASM plugins and the Tool trait.

use crate::PluginCapability;
use crate::component::PluginLimits;
use crate::host::AdmittedComponent;
use crate::instance::PluginInstanceScope;
use crate::runtime;
use crate::services::PluginHostServices;
use async_trait::async_trait;
use serde_json::Value;
use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
use zeroclaw_api::tool::{Tool, ToolResult};

/// A tool backed by a WASM plugin function.
pub struct WasmTool {
    name: String,
    description: String,
    parameters_schema: Value,
    component: AdmittedComponent,
    scope: PluginInstanceScope,
    services: PluginHostServices,
    limits: PluginLimits,
}

impl Attributable for WasmTool {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::Plugin)
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
    /// Build an adapter from already-read metadata and a live host-service bundle.
    pub fn new(
        name: String,
        description: String,
        parameters_schema: Value,
        component: AdmittedComponent,
        scope: PluginInstanceScope,
        services: PluginHostServices,
        limits: PluginLimits,
    ) -> anyhow::Result<Self> {
        scope.require_capability(PluginCapability::Tool)?;
        services.resolve_config(&scope)?;
        Ok(Self {
            name,
            description,
            parameters_schema,
            component,
            scope,
            services,
            limits,
        })
    }

    /// Create a `WasmTool` by loading its required metadata exports.
    ///
    /// Components that cannot be loaded, instantiated, or queried are rejected
    /// instead of being registered with synthetic metadata. `services` must
    /// resolve canonical live config under the supplied instance scope.
    pub fn from_wasm(
        component: AdmittedComponent,
        scope: PluginInstanceScope,
        services: PluginHostServices,
        limits: PluginLimits,
    ) -> anyhow::Result<Self> {
        scope.require_capability(PluginCapability::Tool)?;
        services.resolve_config(&scope)?;
        let probe = {
            let component = component.clone();
            let scope = scope.clone();
            let services = services.clone();
            block_probe(async move {
                let mut plugin =
                    runtime::create_plugin(&component, &scope, &services, limits).await?;
                runtime::call_tool_metadata(&mut plugin).await
            })
        };
        let meta = probe?;

        Ok(Self {
            name: meta.name,
            description: meta.description,
            parameters_schema: meta.parameters_schema,
            component,
            scope,
            services,
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
        self.services.resolve_config(&self.scope)?;
        let mut plugin =
            runtime::create_plugin(&self.component, &self.scope, &self.services, self.limits)
                .await?;
        runtime::call_execute(&mut plugin, &args_json).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PluginConfigResolver;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    fn tool_scope() -> PluginInstanceScope {
        crate::instance::test_scope(PluginCapability::Tool, "redaction-primary", [])
    }

    fn component() -> AdmittedComponent {
        AdmittedComponent::test_component(b"not-a-component")
    }

    #[test]
    fn tool_attribution_keeps_callable_and_instance_identities_distinct() {
        let schema = serde_json::json!({"type": "object", "properties": {}});
        let tool = WasmTool::new(
            "redact".to_string(),
            "does things".to_string(),
            schema.clone(),
            component(),
            tool_scope(),
            crate::services::test_host_services(),
            crate::component::test_limits(1_000),
        )
        .expect("tool scope matches adapter");
        assert_eq!(tool.name(), "redact");
        assert_eq!(tool.description(), "does things");
        assert_eq!(tool.parameters_schema(), schema);
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
            component(),
            scope,
            crate::services::test_host_services(),
            crate::component::test_limits(0),
        );

        assert!(result.is_err());
    }

    #[test]
    fn new_rejects_invalid_config() {
        let services = crate::services::test_services(PluginConfigResolver::new(|_| {
            Err(crate::error::PluginError::InvalidConfig(
                "invalid-constructor-config".to_string(),
            ))
        }));
        let result = WasmTool::new(
            "my_tool".to_string(),
            "does things".to_string(),
            serde_json::json!({}),
            component(),
            tool_scope(),
            services,
            crate::component::test_limits(0),
        );

        assert!(result.is_err());
    }

    #[test]
    fn from_wasm_rejects_invalid_component_bytes() {
        let result = WasmTool::from_wasm(
            component(),
            tool_scope(),
            crate::services::test_host_services(),
            crate::component::test_limits(0),
        );

        assert!(result.is_err());
    }

    #[test]
    fn from_wasm_validates_config_before_loading_guest_code() {
        let services = crate::services::test_services(PluginConfigResolver::new(|_| {
            Err(crate::error::PluginError::InvalidConfig(
                "invalid-before-load".to_string(),
            ))
        }));
        let error = WasmTool::from_wasm(
            component(),
            tool_scope(),
            services,
            crate::component::test_limits(0),
        )
        .err()
        .expect("invalid config must reject registration");

        assert!(error.to_string().contains("invalid-before-load"));
    }

    #[tokio::test]
    async fn execute_revalidates_live_config_before_loading_guest_code() {
        let reject = Arc::new(AtomicBool::new(false));
        let reject_for_resolver = Arc::clone(&reject);
        let services = crate::services::test_services(PluginConfigResolver::new(move |scope| {
            if reject_for_resolver.load(Ordering::Relaxed) {
                return Err(crate::error::PluginError::InvalidConfig(
                    "invalid-before-execute".to_string(),
                ));
            }
            crate::services::test_host_services().resolve_config(scope)
        }));
        let tool = WasmTool::new(
            "my_tool".to_string(),
            "does things".to_string(),
            serde_json::json!({}),
            component(),
            tool_scope(),
            services,
            crate::component::test_limits(0),
        )
        .expect("initial live config must be valid");

        reject.store(true, Ordering::Relaxed);
        let error = tool
            .execute(serde_json::json!({}))
            .await
            .expect_err("live config must be revalidated before loading guest code");

        assert!(error.to_string().contains("invalid-before-execute"));
    }
}
