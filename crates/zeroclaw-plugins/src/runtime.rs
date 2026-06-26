//! Wasmtime component-model execution bridge.
//!
//! Loads a `wasm32-wasip2` component implementing the `tool-plugin` world,
//! registers the `logging` host import, and invokes the exported `tool`
//! interface. Each invocation gets a fresh store; the component itself is
//! compiled once and reused.

use crate::PluginPermission;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};
use zeroclaw_api::tool::ToolResult;

wasmtime::component::bindgen!({
    path: "../../wit/v0",
    world: "tool-plugin",
});

use self::exports::zeroclaw::plugin::tool::ToolResult as WitToolResult;
use self::zeroclaw::plugin::logging::{Host as LoggingHost, LogLevel, PluginEvent};

fn wt<T>(r: wasmtime::Result<T>, ctx: &'static str) -> Result<T> {
    r.map_err(|e| anyhow::Error::msg(format!("{ctx}: {e}")))
}

/// Tool metadata read from a plugin's exported `tool` interface.
#[derive(Debug)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

/// A compiled component.
pub struct Plugin {
    component: Component,
}

/// Per-invocation store state. Carries only what host imports need.
struct PluginState;

impl LoggingHost for PluginState {
    fn log_record(&mut self, level: LogLevel, event: PluginEvent) {
        emit_log(level, event);
    }
}

impl self::zeroclaw::plugin::types::Host for PluginState {}

fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(Engine::default)
}

fn linker() -> Result<Linker<PluginState>> {
    let mut linker = Linker::new(engine());
    let options = LinkOptions::default();
    wt(
        ToolPlugin::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
            &mut linker,
            &options,
            |s| s,
        ),
        "failed to add plugin imports to linker",
    )?;
    Ok(linker)
}

/// Compile a component from a WASM file. Permissions gate config and host
/// access at call time, not at compile time. With a JIT backend present a
/// `.wasm` component is compiled on load; in runtime-only builds the file is a
/// precompiled `.cwasm` deserialized directly.
pub fn create_plugin(wasm_path: &Path, _permissions: &[PluginPermission]) -> Result<Plugin> {
    let component = wt(load_component(wasm_path), "failed to load WASM component")?;
    Ok(Plugin { component })
}

#[cfg(feature = "plugins-wasm-cranelift")]
fn load_component(wasm_path: &Path) -> wasmtime::Result<Component> {
    Component::from_file(engine(), wasm_path)
}

#[cfg(not(feature = "plugins-wasm-cranelift"))]
fn load_component(wasm_path: &Path) -> wasmtime::Result<Component> {
    // SAFETY: the file is a wasmtime-produced `.cwasm` for this engine; a
    // mismatched artifact is rejected by deserialize's version check.
    unsafe { Component::deserialize_file(engine(), wasm_path) }
}

fn instantiate(plugin: &Plugin) -> Result<(Store<PluginState>, ToolPlugin)> {
    let mut store = Store::new(engine(), PluginState);
    let bindings = wt(
        ToolPlugin::instantiate(&mut store, &plugin.component, &linker()?),
        "failed to instantiate plugin component",
    )?;
    Ok((store, bindings))
}

/// Read the exported tool's metadata.
pub fn call_tool_metadata(plugin: &mut Plugin) -> Result<ToolMetadata> {
    let (mut store, bindings) = instantiate(plugin)?;
    let tool = bindings.zeroclaw_plugin_tool();
    let name = wt(tool.call_name(&mut store), "tool.name failed")?;
    let description = wt(tool.call_description(&mut store), "tool.description failed")?;
    let schema_json = wt(
        tool.call_parameters_schema(&mut store),
        "tool.parameters-schema failed",
    )?;
    let parameters_schema =
        serde_json::from_str(&schema_json).context("tool parameters-schema is not valid JSON")?;
    Ok(ToolMetadata {
        name,
        description,
        parameters_schema,
    })
}

/// Invoke the exported tool's `execute`, injecting the plugin's resolved config.
pub fn call_execute(
    plugin: &mut Plugin,
    args_json: &[u8],
    config: &HashMap<String, String>,
    permissions: &[PluginPermission],
) -> Result<ToolResult> {
    let input = inject_config(args_json, effective_config(config, permissions))?;
    let (mut store, bindings) = instantiate(plugin)?;
    let result = wt(
        bindings
            .zeroclaw_plugin_tool()
            .call_execute(&mut store, &input),
        "tool.execute trapped",
    )?
    .map_err(|e| anyhow::Error::msg(format!("plugin execute returned error: {e}")))?;
    Ok(into_tool_result(result))
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

fn emit_log(level: LogLevel, event: PluginEvent) {
    let message = format!("[{}] {}", event.function_name, event.message);
    match level {
        LogLevel::Trace | LogLevel::Debug => zeroclaw_log::record!(
            DEBUG,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            &message
        ),
        LogLevel::Info => zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            &message
        ),
        LogLevel::Warn | LogLevel::Error => zeroclaw_log::record!(
            WARN,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                .with_outcome(zeroclaw_log::EventOutcome::Unknown),
            &message
        ),
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

    #[test]
    fn missing_wasm_file_returns_error() {
        assert!(create_plugin(Path::new("/nonexistent/plugin.wasm"), &[]).is_err());
    }
}
