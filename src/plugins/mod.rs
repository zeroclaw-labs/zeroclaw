//! Plugin system for ZeroClaw.
//!
//! Modeled after OpenClaw's plugin architecture, adapted for Rust:
//!
//! - **Manifest**: each plugin has a `zeroclaw.plugin.toml` descriptor
//! - **Discovery**: scans bundled, global (`~/.zeroclaw/extensions/`), and
//!   workspace (`.zeroclaw/extensions/`) directories
//! - **Registry**: collects loaded plugins, their tools, hooks, and diagnostics
//! - **PluginApi**: passed to `Plugin::register()` so plugins can register
//!   tools, hooks, and services without knowing the host internals
//! - **Error isolation**: panics inside plugin `register()` are caught and
//!   recorded as diagnostics rather than crashing the host
//!
//! # Quick start
//!
//! ```rust,ignore
//! use zeroclaw::plugins::{Plugin, PluginApi, PluginManifest};
//!
//! pub struct MyPlugin { manifest: PluginManifest }
//!
//! impl Plugin for MyPlugin {
//!     fn manifest(&self) -> &PluginManifest { &self.manifest }
//!     fn register(&self, api: &mut PluginApi) -> anyhow::Result<()> {
//!         api.register_tool(Box::new(MyTool));
//!         Ok(())
//!     }
//! }
//! ```
//!
//! Then in your `config.toml`:
//!
//! ```toml
//! [plugins]
//! enabled = true
//!
//! [plugins.entries.my-plugin]
//! enabled = true
//! ```

pub mod bridge;
pub mod discovery;
pub mod loader;
pub mod manifest;
pub mod registry;
pub mod runtime;
pub mod traits;

#[allow(unused_imports)]
pub use discovery::discover_plugins;
#[allow(unused_imports)]
pub use loader::load_plugins;
#[allow(unused_imports)]
pub use manifest::{PluginManifest, PLUGIN_MANIFEST_FILENAME};
#[allow(unused_imports)]
pub use registry::{
    DiagnosticLevel, PluginDiagnostic, PluginHookRegistration, PluginOrigin, PluginRecord,
    PluginRegistry, PluginStatus, PluginToolRegistration,
};
#[allow(unused_imports)]
pub use traits::{Plugin, PluginApi, PluginCapability, PluginLogger};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_reexports_are_accessible() {
        let _manifest = PluginManifest {
            id: "test".into(),
            name: None,
            description: None,
            version: None,
            config_schema: None,
            capabilities: vec![],
            module_path: String::new(),
            wit_packages: vec![],
            tools: vec![],
            providers: vec![],
        };
        assert_eq!(PLUGIN_MANIFEST_FILENAME, "zeroclaw.plugin.toml");
    }
}
