#![cfg(feature = "plugins-wasm")]

//! Verify that `[plugin.capabilities]` is parsed from plugin.toml.
//!
//! Acceptance criterion for US-ZCL-22:
//! > [plugin.capabilities] section parsed from plugin.toml

use zeroclaw::plugins::{PluginCapability, PluginManifest};

#[test]
fn capabilities_parsed_from_nested_plugin_toml_all_variants() {
    let toml_str = r#"
[plugin]
name = "all-caps"
version = "1.0.0"
wasm_path = "plugin.wasm"
capabilities = ["tool", "channel", "memory", "observer"]
"#;
    let manifest = PluginManifest::parse(toml_str).unwrap();

    assert_eq!(
        manifest.capabilities,
        vec![
            PluginCapability::Tool,
            PluginCapability::Channel,
            PluginCapability::Memory,
            PluginCapability::Observer,
        ]
    );
}

#[test]
fn capabilities_parsed_from_nested_plugin_toml_single() {
    let toml_str = r#"
[plugin]
name = "single-cap"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["channel"]
"#;
    let manifest = PluginManifest::parse(toml_str).unwrap();

    assert_eq!(manifest.capabilities, vec![PluginCapability::Channel]);
}

#[test]
fn capabilities_parsed_from_nested_plugin_toml_empty() {
    let toml_str = r#"
[plugin]
name = "no-caps"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = []
"#;
    let manifest = PluginManifest::parse(toml_str).unwrap();

    assert!(manifest.capabilities.is_empty());
}

#[test]
fn capabilities_missing_from_nested_plugin_toml_errors() {
    let toml_str = r#"
[plugin]
name = "broken"
version = "0.1.0"
wasm_path = "plugin.wasm"
"#;
    let err = PluginManifest::parse(toml_str).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("capabilities") || msg.to_lowercase().contains("missing"),
        "error should indicate missing capabilities: {msg}"
    );
}

#[test]
fn capabilities_parsed_from_flat_plugin_toml() {
    let toml_str = r#"
name = "flat-caps"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool", "memory"]
"#;
    let manifest = PluginManifest::parse(toml_str).unwrap();

    assert_eq!(
        manifest.capabilities,
        vec![PluginCapability::Tool, PluginCapability::Memory]
    );
}

#[test]
fn capabilities_invalid_variant_errors() {
    let toml_str = r#"
[plugin]
name = "bad-cap"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["nonexistent"]
"#;
    assert!(
        PluginManifest::parse(toml_str).is_err(),
        "unknown capability variant should fail parsing"
    );
}
