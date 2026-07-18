//! Tool plugin execution: bridges the `tool-plugin` world to the runtime's
//! `ToolMetadata`/`ToolResult` surface. Fresh store per call, stateless.

use crate::component::bindings::tool::ToolPlugin;
use crate::component::bindings::tool::exports::zeroclaw::plugin::tool::ToolResult as WitToolResult;
use crate::component::{
    PluginState, PluginStoreSpec, call_plugin, call_store, call_tool_execute, engine,
    load_component, wt,
};
use crate::host::AdmittedComponent;
use crate::instance::PluginInstanceScope;
use crate::services::PluginHostServices;
use crate::{PluginCapability, PluginPermission};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::Mutex;
use wasmtime::Store;
use wasmtime::component::Linker;
use zeroclaw_api::tool::ToolResult;

/// Tool metadata read from a plugin's exported `tool` interface.
#[derive(Debug)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

/// A warm tool plugin: store and bindings created once, reused per call.
pub struct Plugin {
    state: Arc<Mutex<(Store<PluginState>, ToolPlugin)>>,
}

/// Build exactly the optional imports authorized by this store's admitted
/// scope. Linkers are materialized once per plugin load so adding another
/// optional interface cannot create a combinatorial cache of variants.
fn build_linker(state: &PluginState) -> Result<Linker<PluginState>> {
    let mut linker = Linker::new(engine());
    crate::component::add_wasi(&mut linker)?;
    if state.http_enabled() {
        crate::component::add_wasi_http(&mut linker)?;
    }
    let mut options = crate::component::bindings::tool::LinkOptions::default();
    options.plugins_wit_v0(true);
    options.plugins_wit_v0_sockets(state.permission_enabled(PluginPermission::SocketClient));
    options.plugins_wit_v0_websocket(state.permission_enabled(PluginPermission::WebSocketClient));
    wt(
        ToolPlugin::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
            &mut linker,
            &options,
            |s| s,
        ),
        "failed to add tool plugin imports to linker",
    )?;
    Ok(linker)
}

/// Compile and instantiate a tool plugin under one host-issued scope.
///
/// The scope decides whether the store/linker expose outbound HTTP, WebSocket,
/// and socket imports. Deriving them from the same admission prevents authority
/// from drifting between instantiation and execution. The required service
/// bundle resolves canonical live config for that same scope.
pub async fn create_plugin(
    component: &AdmittedComponent,
    scope: &PluginInstanceScope,
    services: &PluginHostServices,
    limits: crate::component::PluginLimits,
) -> Result<Plugin> {
    scope.require_capability(PluginCapability::Tool)?;
    let component = load_component(component)?;
    let mut store = crate::component::new_store(
        PluginStoreSpec::new(scope.clone(), services.clone(), limits).with_granted_http(),
    );
    let linker = build_linker(store.data())?;
    let bindings: Result<_> = call_store!(store, async |store: &mut Store<PluginState>| {
        wt(
            ToolPlugin::instantiate_async(store, &component, &linker).await,
            "failed to instantiate tool plugin",
        )
    });
    Ok(Plugin {
        state: Arc::new(Mutex::new((store, bindings?))),
    })
}

/// Read the exported tool's metadata.
pub async fn call_tool_metadata(plugin: &mut Plugin) -> Result<ToolMetadata> {
    call_plugin!(
        plugin,
        async move |store: &mut Store<PluginState>, bindings: &mut ToolPlugin| {
            let tool = bindings.zeroclaw_plugin_tool();
            let name = wt(tool.call_name(&mut *store).await, "tool.name failed")?;
            let description = wt(
                tool.call_description(&mut *store).await,
                "tool.description failed",
            )?;
            let schema_json = wt(
                tool.call_parameters_schema(&mut *store).await,
                "tool.parameters-schema failed",
            )?;
            let parameters_schema = serde_json::from_str(&schema_json)
                .context("tool parameters-schema is not valid JSON")?;
            Ok(ToolMetadata {
                name,
                description,
                parameters_schema,
            })
        }
    )
}

/// Invoke the exported tool's `execute`, injecting its non-secret resolved config.
pub async fn call_execute(plugin: &mut Plugin, args_json: &[u8]) -> Result<ToolResult> {
    call_tool_execute!(
        plugin,
        async move |store: &mut Store<PluginState>, bindings: &mut ToolPlugin| {
            let config = store.data_mut().public_config()?;
            let input = inject_config(args_json, &config)?;
            let result = wt(
                bindings
                    .zeroclaw_plugin_tool()
                    .call_execute(store, &input)
                    .await,
                "tool.execute trapped",
            )?
            .map_err(|e| anyhow::Error::msg(format!("plugin execute returned error: {e}")))?;
            Ok(into_tool_result(result))
        }
    )
}

fn into_tool_result(result: WitToolResult) -> ToolResult {
    ToolResult {
        success: result.success,
        output: result.output.into(),
        error: result.error,
    }
}

/// Merge the plugin's public resolved config under the reserved `__config` key,
/// stripping any caller-supplied `__config` so the section cannot be spoofed.
fn inject_config(args_json: &[u8], config: &serde_json::Value) -> Result<String> {
    let mut args: serde_json::Value =
        serde_json::from_slice(args_json).context("plugin args are not valid JSON")?;
    let obj = args
        .as_object_mut()
        .context("plugin args must be a JSON object")?;
    obj.remove("__config");
    if config.as_object().is_some_and(|config| !config.is_empty()) {
        obj.insert("__config".to_string(), config.clone());
    }
    serde_json::to_string(&args).context("failed to serialize plugin input")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_config_adds_public_config_key() {
        let config = serde_json::json!({"region": "west"});
        let out = inject_config(br#"{"prompt":"a sunset"}"#, &config).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["prompt"], "a sunset");
        assert_eq!(v["__config"]["region"], "west");
    }

    #[test]
    fn inject_config_empty_leaves_args_untouched() {
        let out = inject_config(br#"{"prompt":"x"}"#, &serde_json::json!({})).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("__config").is_none());
    }

    #[test]
    fn inject_config_rejects_non_object_args() {
        let config = serde_json::json!({"k": "v"});
        assert!(inject_config(br#"[1,2,3]"#, &config).is_err());
    }

    #[test]
    fn inject_config_strips_caller_supplied_config_when_section_empty() {
        let out = inject_config(
            br#"{"prompt":"x","__config":{"region":"forged"}}"#,
            &serde_json::json!({}),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("__config").is_none());
        assert_eq!(v["prompt"], "x");
    }

    #[test]
    fn inject_config_overrides_caller_supplied_config_when_section_present() {
        let config = serde_json::json!({"region": "real"});
        let out =
            inject_config(br#"{"prompt":"x","__config":{"region":"forged"}}"#, &config).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["__config"]["region"], "real");
    }

    #[test]
    fn inject_config_preserves_typed_values() {
        let config = serde_json::json!({
            "enabled": true,
            "limit": 5,
            "labels": ["one", "two"],
            "nested": {"ratio": 1.5}
        });
        let out = inject_config(br#"{"prompt":"x"}"#, &config).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(value["__config"], config);
    }

    #[tokio::test]
    async fn create_plugin_rejects_a_scope_for_another_capability() {
        let scope = crate::instance::test_scope(PluginCapability::Channel, "main", []);
        let component = AdmittedComponent::test_component(b"not-a-component");
        let result = create_plugin(
            &component,
            &scope,
            &crate::services::test_host_services(),
            crate::component::test_limits(0),
        )
        .await;

        let error = result.err().expect("capability mismatch must fail");
        assert!(format!("{error:#}").contains("capability"));
    }
}
