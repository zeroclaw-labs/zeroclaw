//! ToolRegistry — central store of all available tools.
//!
//! The LLM receives its tool list exclusively from the registry.
//! If a tool is not registered, the LLM cannot call it.
//!
//! Startup sequence (called via [`ToolRegistry::load`]):
//! 1. Register built-in hardware tools (`gpio_read`, `gpio_write`).
//! 2. Scan `~/.zeroclaw/tools/` for user plugin manifests.
//! 3. Build a `SubprocessTool` for each valid manifest and register it.
//! 4. Print the startup log summarising loaded tools and connected devices.
//!
//! Dispatch flow (called per LLM tool-call):
//! ```text
//! registry.dispatch("gpio_write", {"device":"pico0","pin":25,"value":1})
//!     │
//!     ├── look up "gpio_write" in tools HashMap
//!     └── tool.execute(args) → ToolResult
//! ```
//!
//! Device lookup is handled internally by each tool (GPIO tools read the
//! [`DeviceRegistry`] themselves via their `Arc<RwLock<DeviceRegistry>>`).

use super::device::DeviceRegistry;
use super::gpio::gpio_tools;
use super::loader::scan_plugin_dir;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use zeroclaw_api::tool::{Tool, ToolResult};

// ── ToolError ─────────────────────────────────────────────────────────────────

/// Error type returned by [`ToolRegistry::dispatch`].
#[derive(Debug, Error)]
pub enum ToolError {
    /// No tool with the requested name is registered.
    #[error("unknown tool: '{0}'")]
    UnknownTool(String),

    /// The tool's `execute` method returned an error.
    #[error("tool execution failed: {0}")]
    ExecutionFailed(String),
}

// ── ToolRegistry ──────────────────────────────────────────────────────────────

/// Central registry of all available tools (built-ins + user plugins).
///
/// Cheaply cloneable via the inner `Arc` — wrapping in an outer `Arc` is not
/// needed in most call sites.
pub struct ToolRegistry {
    /// Map of tool name → boxed `Tool` impl.
    tools: HashMap<String, Box<dyn Tool>>,
    /// Shared device registry — retained for future introspection / hot-reload.
    device_registry: Arc<RwLock<DeviceRegistry>>,
}

impl ToolRegistry {
    /// Load the registry at startup.
    ///
    /// 1. Instantiates the built-in GPIO tools.
    /// 2. Scans `~/.zeroclaw/tools/` for user plugins and registers each one.
    /// 3. Prints the startup log.
    ///
    /// Plugin loading errors are logged as warnings and never abort startup.
    pub async fn load(devices: Arc<RwLock<DeviceRegistry>>) -> anyhow::Result<Self> {
        let mut tools: HashMap<String, Box<dyn Tool>> = HashMap::new();

        // ── 1. Built-in tools ─────────────────────────────────────────────
        for tool in gpio_tools(devices.clone()) {
            let name = tool.name().to_string();
            if tools.contains_key(&name) {
                anyhow::bail!("duplicate built-in tool name: '{}'", name);
            }
            println!("[registry] loaded built-in: {}", name);
            tools.insert(name, tool);
        }

        // pico_flash — hardware feature only (needs UF2 assets embedded at compile time)
        #[cfg(feature = "hardware")]
        {
            let tool: Box<dyn Tool> =
                Box::new(super::pico_flash::PicoFlashTool::new(devices.clone()));
            let name = tool.name().to_string();
            if tools.contains_key(&name) {
                anyhow::bail!("duplicate built-in tool name: '{}'", name);
            }
            println!("[registry] loaded built-in: {}", name);
            tools.insert(name, tool);
        }

        // Phase 7: dynamic code tools (device_read_code, device_write_code, device_exec)
        #[cfg(feature = "hardware")]
        {
            for tool in super::pico_code::device_code_tools(devices.clone()) {
                let name = tool.name().to_string();
                if tools.contains_key(&name) {
                    anyhow::bail!("duplicate built-in tool name: '{}'", name);
                }
                println!("[registry] loaded built-in: {}", name);
                tools.insert(name, tool);
            }
        }

        // Aardvark I2C / SPI / GPIO tools + datasheet tool (hardware feature only,
        // and only when at least one Aardvark adapter is present at startup).
        #[cfg(feature = "hardware")]
        {
            let has_aardvark = {
                let reg = devices.read().await;
                reg.has_aardvark()
            };
            if has_aardvark {
                for tool in super::aardvark_tools::aardvark_tools(devices.clone()) {
                    let name = tool.name().to_string();
                    if tools.contains_key(&name) {
                        anyhow::bail!("duplicate built-in tool name: '{}'", name);
                    }
                    println!("[registry] loaded built-in: {}", name);
                    tools.insert(name, tool);
                }
                // Datasheet tool: always useful once an Aardvark is connected.
                {
                    let tool: Box<dyn Tool> = Box::new(super::datasheet::DatasheetTool::new());
                    let name = tool.name().to_string();
                    if tools.contains_key(&name) {
                        anyhow::bail!("duplicate built-in tool name: '{}'", name);
                    }
                    println!("[registry] loaded built-in: {}", name);
                    tools.insert(name, tool);
                }
            }
        }

        // ── 2. User plugins ───────────────────────────────────────────────
        let plugins = scan_plugin_dir();
        for plugin in plugins {
            if tools.contains_key(&plugin.name) {
                anyhow::bail!(
                    "duplicate tool name: plugin '{}' conflicts with an existing tool",
                    plugin.name
                );
            }
            println!(
                "[registry] loaded plugin: {} (v{})",
                plugin.name, plugin.version
            );
            tools.insert(plugin.name, plugin.tool);
        }

        // ── 3. Startup summary ────────────────────────────────────────────
        println!("[registry] {} tools available", tools.len());

        {
            let reg = devices.read().await;
            let mut aliases = reg.aliases();
            aliases.sort_unstable(); // deterministic log order
            for alias in aliases {
                if let Some(device) = reg.get_device(alias) {
                    let port = device.port().unwrap_or("(native)");
                    println!("[registry] {} ready → {}", alias, port);
                }
            }
        }

        Ok(Self {
            tools,
            device_registry: devices,
        })
    }

    /// Returns a JSON Schema array for **all** registered tools.
    ///
    /// Each element follows the shape the LLM expects for function calling:
    /// ```json
    /// {
    ///   "name": "gpio_write",
    ///   "description": "...",
    ///   "parameters": { "type": "object", "properties": { ... }, "required": [...] }
    /// }
    /// ```
    ///
    /// Inject the result of this method into the LLM system prompt so the
    /// model knows what tools exist and how to call them.
    pub fn schemas(&self) -> Vec<serde_json::Value> {
        let mut schemas: Vec<serde_json::Value> = self
            .tools
            .values()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters_schema(),
                })
            })
            .collect();

        // Sort by name for deterministic output (important for prompt stability).
        schemas.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        });

        schemas
    }

    /// Dispatch a tool call from the LLM.
    ///
    /// Looks up the tool by `name` and delegates to `tool.execute(args)`.
    /// Returns [`ToolError::UnknownTool`] when no matching tool is found.
    pub async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::UnknownTool(name.to_string()))?;

        tool.execute(args)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    /// List all registered tool names (sorted, for logging / debug).
    pub fn list(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the registry contains no tools.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Borrow the device registry (e.g. for introspection or hot-reload).
    pub fn device_registry(&self) -> Arc<RwLock<DeviceRegistry>> {
        self.device_registry.clone()
    }

    /// Consume the registry and return all tools as a `Vec`.
    ///
    /// Used by [`crate::boot`] to hand tools off to the agent loop,
    /// which manages its own flat `Vec<Box<dyn Tool>>` registry.
    /// Order is alphabetical by tool name for deterministic output.
    pub fn into_tools(self) -> Vec<Box<dyn Tool>> {
        let mut pairs: Vec<(String, Box<dyn Tool>)> = self.tools.into_iter().collect();
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        pairs.into_iter().map(|(_, tool)| tool).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an empty DeviceRegistry behind the expected Arc<RwLock<…>>.
    fn empty_device_registry() -> Arc<RwLock<DeviceRegistry>> {
        Arc::new(RwLock::new(DeviceRegistry::new()))
    }

    #[tokio::test]
    async fn load_registers_builtin_gpio_tools() {
        let devices = empty_device_registry();
        let registry = ToolRegistry::load(devices).await.expect("load failed");

        let names = registry.list();
        assert!(
            names.contains(&"gpio_write"),
            "gpio_write missing; got: {:?}",
            names
        );
        assert!(
            names.contains(&"gpio_read"),
            "gpio_read missing; got: {:?}",
            names
        );
        assert!(registry.len() >= 2);
    }

    /// With the `hardware` feature, exactly 6 built-in tools must be present:
    /// gpio_read, gpio_write, pico_flash, device_read_code, device_write_code, device_exec.
    #[cfg(feature = "hardware")]
    #[tokio::test]
    async fn hardware_feature_registers_all_six_tools() {
        let devices = empty_device_registry();
        let registry = ToolRegistry::load(devices).await.expect("load failed");

        let names = registry.list();
        let expected = [
            "device_exec",
            "device_read_code",
            "device_write_code",
            "gpio_read",
            "gpio_write",
            "pico_flash",
        ];
        for tool_name in &expected {
            assert!(
                names.contains(tool_name),
                "expected tool '{}' missing; got: {:?}",
                tool_name,
                names
            );
        }
        assert_eq!(
            registry.len(),
            6,
            "expected exactly 6 built-in tools, got {} (names: {:?})",
            registry.len(),
            names
        );
    }

    #[tokio::test]
    async fn schemas_returns_valid_json_schema_array() {
        let devices = empty_device_registry();
        let registry = ToolRegistry::load(devices).await.expect("load failed");

        let schemas = registry.schemas();
        assert!(!schemas.is_empty());

        for schema in &schemas {
            assert!(schema["name"].is_string(), "name missing in schema");
            assert!(schema["description"].is_string(), "description missing");
            assert!(
                schema["parameters"]["type"] == "object",
                "parameters.type should be object"
            );
        }
    }

    #[tokio::test]
    async fn schemas_are_sorted_by_name() {
        let devices = empty_device_registry();
        let registry = ToolRegistry::load(devices).await.expect("load failed");

        let schemas = registry.schemas();
        let names: Vec<&str> = schemas
            .iter()
            .map(|s| s["name"].as_str().unwrap_or(""))
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "schemas not sorted by name");
    }

    #[tokio::test]
    async fn dispatch_unknown_tool_returns_error() {
        let devices = empty_device_registry();
        let registry = ToolRegistry::load(devices).await.expect("load failed");

        let result = registry
            .dispatch("nonexistent_tool", serde_json::json!({}))
            .await;

        match result {
            Err(ToolError::UnknownTool(name)) => assert_eq!(name, "nonexistent_tool"),
            other => panic!("expected UnknownTool, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn list_returns_sorted_tool_names() {
        let devices = empty_device_registry();
        let registry = ToolRegistry::load(devices).await.expect("load failed");

        let names = registry.list();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "list() should return sorted names; got: {:?}",
            names
        );
    }

    #[test]
    fn tool_error_display() {
        let e = ToolError::UnknownTool("bad_tool".to_string());
        assert_eq!(e.to_string(), "unknown tool: 'bad_tool'");

        let e = ToolError::ExecutionFailed("oops".to_string());
        assert_eq!(e.to_string(), "tool execution failed: oops");
    }
}
