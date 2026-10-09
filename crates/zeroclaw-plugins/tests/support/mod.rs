use std::path::Path;

use tempfile::tempdir;
use zeroclaw_plugins::host::{AdmittedComponent, PluginHost};
use zeroclaw_plugins::{PluginCapability, PluginManifest};

mod state;
pub use state::state_service;

pub fn admit_fixture(path: &Path, manifest: &PluginManifest) -> AdmittedComponent {
    let root = tempdir().expect("create fixture package root");
    let plugin_dir = root.path().join(&manifest.name);
    std::fs::create_dir_all(&plugin_dir).expect("create fixture package directory");
    let relative = manifest
        .wasm_path
        .as_deref()
        .expect("executable fixture declares wasm_path");
    let destination = plugin_dir.join(relative);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).expect("create fixture payload parent");
    }
    std::fs::copy(path, destination).expect("copy fixture payload into package");
    let manifest_toml = toml::to_string(manifest).expect("serialize fixture manifest");
    std::fs::write(plugin_dir.join("manifest.toml"), manifest_toml)
        .expect("write fixture manifest");

    if manifest.capabilities.contains(&PluginCapability::Memory) {
        // The host lists no memory packages yet, so admit this one the way
        // `plugin install` admits a source, into a host with nothing installed.
        let plugins = tempdir().expect("create empty plugins root");
        let host = PluginHost::from_plugins_dir(plugins.path()).expect("open empty plugin host");
        let source = plugin_dir.to_str().expect("fixture package path is UTF-8");
        let admitted = host.admit_source(source).expect("admit fixture package");
        return admitted
            .component()
            .expect("a memory fixture ships a component")
            .clone();
    }

    let host = PluginHost::from_plugins_dir(root.path()).expect("admit fixture package");
    let details = if manifest.capabilities.contains(&PluginCapability::Tool) {
        host.tool_plugin_details()
    } else if manifest.capabilities.contains(&PluginCapability::Channel) {
        host.channel_plugin_details()
    } else {
        panic!("fixture helper supports tool, channel, and memory components")
    };
    assert_eq!(details.len(), 1, "fixture package must be admitted once");
    details[0].1.clone()
}
