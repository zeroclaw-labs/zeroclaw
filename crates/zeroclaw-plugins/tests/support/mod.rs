use std::path::Path;

use tempfile::tempdir;
use zeroclaw_plugins::host::{AdmittedComponent, PluginHost};
use zeroclaw_plugins::{PluginCapability, PluginManifest};

mod egress;
mod state;
pub use egress::egress_service;
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

    let host = PluginHost::from_plugins_dir(root.path()).expect("admit fixture package");
    let details = if manifest.capabilities.contains(&PluginCapability::Tool) {
        host.tool_plugin_details()
    } else if manifest.capabilities.contains(&PluginCapability::Channel) {
        host.channel_plugin_details()
    } else {
        panic!("fixture helper supports tool and channel components")
    };
    assert_eq!(details.len(), 1, "fixture package must be admitted once");
    details[0].1.clone()
}
