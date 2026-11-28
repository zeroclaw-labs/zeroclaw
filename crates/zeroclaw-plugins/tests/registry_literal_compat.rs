//! External-crate baseline public literal compatibility (written, unrun).
use zeroclaw_plugins::registry::{PluginRegistryEntry, PluginRegistryIndex};
#[test]
fn baseline_registry_literals_remain_source_compatible() {
    let entry = PluginRegistryEntry {
        name: "calendar".into(),
        version: "1".into(),
        description: None,
        author: None,
        capabilities: vec![],
        url: "https://example.invalid/p.zip".into(),
        sha256: None,
    };
    let index = PluginRegistryIndex {
        plugins: vec![entry],
        registry_url: None,
    };
    assert_eq!(index.plugins.len(), 1);
}
