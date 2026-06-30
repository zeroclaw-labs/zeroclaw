//! Tool plugin execution: bridges the `tool-plugin` world to the runtime's
//! `ToolMetadata`/`ToolResult` surface. Fresh store per call, stateless.

use crate::PluginPermission;
use crate::component::bindings::tool::ToolPlugin;
use crate::component::bindings::tool::exports::zeroclaw::plugin::tool::ToolResult as WitToolResult;
use crate::component::{PluginState, call_plugin, engine, load_component, wt};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
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

fn linker() -> Result<Linker<PluginState>> {
    let mut linker = Linker::new(engine());
    crate::component::add_wasi(&mut linker)?;
    let mut options = crate::component::bindings::tool::LinkOptions::default();
    options.plugins_wit_v0(true);
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

fn tool_linker() -> &'static Linker<PluginState> {
    static LINKER: OnceLock<Linker<PluginState>> = OnceLock::new();
    LINKER.get_or_init(|| linker().expect("tool linker"))
}

/// Compile and instantiate a tool plugin. Permissions gate config at call time.
pub async fn create_plugin(
    wasm_path: &Path,
    _permissions: &[PluginPermission],
    limits: crate::component::PluginLimits,
) -> Result<Plugin> {
    let component = load_component(wasm_path)?;
    let mut store = crate::component::new_store(limits);
    let bindings = wt(
        ToolPlugin::instantiate_async(&mut store, &component, tool_linker()).await,
        "failed to instantiate tool plugin",
    )?;
    Ok(Plugin {
        state: Arc::new(Mutex::new((store, bindings))),
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

/// Invoke the exported tool's `execute`, injecting the plugin's resolved config.
pub async fn call_execute(
    plugin: &mut Plugin,
    args_json: &[u8],
    config: &HashMap<String, String>,
    permissions: &[PluginPermission],
) -> Result<ToolResult> {
    let input = inject_config(args_json, effective_config(config, permissions))?;
    call_plugin!(
        plugin,
        async move |store: &mut Store<PluginState>, bindings: &mut ToolPlugin| {
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
        output: result.output,
        error: result.error,
    }
}

/// Merge the plugin's resolved config under the reserved `__config` key,
/// stripping any caller-supplied `__config` so the section cannot be spoofed.
fn inject_config(args_json: &[u8], config: &HashMap<String, String>) -> Result<String> {
    let mut args: serde_json::Value =
        serde_json::from_slice(args_json).context("plugin args are not valid JSON")?;
    let obj = args
        .as_object_mut()
        .context("plugin args must be a JSON object")?;
    obj.remove("__config");
    if !config.is_empty() {
        obj.insert(
            "__config".to_string(),
            serde_json::to_value(config).context("failed to serialize plugin config")?,
        );
    }
    serde_json::to_string(&args).context("failed to serialize plugin input")
}

/// The configured section only when the manifest grants `ConfigRead`, else empty.
fn effective_config<'a>(
    config: &'a HashMap<String, String>,
    permissions: &[PluginPermission],
) -> &'a HashMap<String, String> {
    static EMPTY: OnceLock<HashMap<String, String>> = OnceLock::new();
    if permissions.contains(&PluginPermission::ConfigRead) {
        config
    } else {
        EMPTY.get_or_init(HashMap::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_config_adds_config_key() {
        let config = HashMap::from([("api_key".to_string(), "secret".to_string())]);
        let out = inject_config(br#"{"prompt":"a sunset"}"#, &config).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["prompt"], "a sunset");
        assert_eq!(v["__config"]["api_key"], "secret");
    }

    #[test]
    fn inject_config_empty_leaves_args_untouched() {
        let out = inject_config(br#"{"prompt":"x"}"#, &HashMap::new()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("__config").is_none());
    }

    #[test]
    fn inject_config_rejects_non_object_args() {
        let config = HashMap::from([("k".to_string(), "v".to_string())]);
        assert!(inject_config(br#"[1,2,3]"#, &config).is_err());
    }

    #[test]
    fn inject_config_strips_caller_supplied_config_when_section_empty() {
        let out = inject_config(
            br#"{"prompt":"x","__config":{"api_key":"forged"}}"#,
            &HashMap::new(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("__config").is_none());
        assert_eq!(v["prompt"], "x");
    }

    #[test]
    fn inject_config_overrides_caller_supplied_config_when_section_present() {
        let config = HashMap::from([("api_key".to_string(), "real".to_string())]);
        let out = inject_config(
            br#"{"prompt":"x","__config":{"api_key":"forged"}}"#,
            &config,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["__config"]["api_key"], "real");
    }

    #[test]
    fn effective_config_withholds_section_without_config_read() {
        let config = HashMap::from([("api_key".to_string(), "secret".to_string())]);
        let resolved = effective_config(&config, &[PluginPermission::HttpClient]);
        assert!(resolved.is_empty());
    }

    #[test]
    fn effective_config_passes_section_with_config_read() {
        let config = HashMap::from([("api_key".to_string(), "secret".to_string())]);
        let resolved = effective_config(&config, &[PluginPermission::ConfigRead]);
        assert_eq!(resolved.get("api_key").map(String::as_str), Some("secret"));
    }
}
