//! Baseline public DTO literals remain constructible by an external crate.
use zeroclaw_gateway::api_plugins::{AvailablePluginPackage, PluginCatalogEntry, PluginsResponse};
#[test]
fn baseline_gateway_literals_remain_source_compatible() {
    let available = AvailablePluginPackage {
        version: "1".into(),
        description: None,
        capabilities: vec![],
        install_source: "calendar@1".into(),
    };
    let entry = PluginCatalogEntry {
        name: "calendar".into(),
        installed: None,
        available: Some(available),
    };
    let response = PluginsResponse {
        plugins_enabled: true,
        wasm_plugins_available: true,
        plugins_dir: "plugins".into(),
        plugins: vec![entry],
        issues: vec![],
    };
    assert_eq!(response.plugins.len(), 1);
}
