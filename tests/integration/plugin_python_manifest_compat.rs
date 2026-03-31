//! Verify that the Python echo plugin's plugin.toml uses the same manifest
//! format as Rust plugins and parses without host-side changes.
//!
//! Acceptance criterion for US-ZCL-34:
//! > plugin.toml uses the same manifest format as Rust plugins (no host-side changes)

use std::path::Path;
use zeroclaw::plugins::{PluginCapability, PluginManifest, RiskLevel};

/// The Python echo plugin's plugin.toml must parse through the same
/// `PluginManifest::parse` codepath used by all Rust plugins.
#[test]
fn python_echo_plugin_toml_parses_with_standard_manifest_parser() {
    let manifest_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins/python-echo-plugin/plugin.toml");
    let content = std::fs::read_to_string(&manifest_path)
        .expect("python-echo-plugin/plugin.toml should exist");

    let manifest = PluginManifest::parse(&content)
        .expect("python-echo-plugin/plugin.toml should parse without errors");

    assert_eq!(manifest.name, "python-echo-plugin");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(manifest.wasm_path, "python_echo_plugin.wasm");
    assert_eq!(manifest.capabilities, vec![PluginCapability::Tool]);
}

/// The Python echo plugin must declare a tool_echo tool with the same schema
/// structure as the Rust echo plugin.
#[test]
fn python_echo_plugin_declares_tool_echo_matching_rust_format() {
    let manifest_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins/python-echo-plugin/plugin.toml");
    let content = std::fs::read_to_string(&manifest_path).unwrap();
    let manifest = PluginManifest::parse(&content).unwrap();

    assert_eq!(manifest.tools.len(), 1, "should declare exactly one tool");

    let tool = &manifest.tools[0];
    assert_eq!(tool.name, "tool_echo");
    assert_eq!(tool.export, "tool_echo");
    assert_eq!(tool.risk_level, RiskLevel::Low);
    assert!(
        tool.parameters_schema.is_some(),
        "tool should have a parameters_schema"
    );
}

/// Both the Python and Rust echo plugins must produce identical manifest
/// structures (modulo name/description/wasm_path), proving no host-side
/// format changes are needed for Python plugins.
#[test]
fn python_and_rust_echo_manifests_use_identical_format() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins");

    let py_content = std::fs::read_to_string(base.join("python-echo-plugin/plugin.toml")).unwrap();
    let rs_content = std::fs::read_to_string(base.join("echo-plugin/plugin.toml")).unwrap();

    let py = PluginManifest::parse(&py_content).unwrap();
    let rs = PluginManifest::parse(&rs_content).unwrap();

    // Structural equivalence: same capabilities, permissions, tool count, tool schema
    assert_eq!(py.capabilities, rs.capabilities, "capabilities must match");
    assert_eq!(py.permissions, rs.permissions, "permissions must match");
    assert_eq!(py.tools.len(), rs.tools.len(), "tool count must match");

    let py_tool = &py.tools[0];
    let rs_tool = &rs.tools[0];
    assert_eq!(py_tool.name, rs_tool.name, "tool names must match");
    assert_eq!(py_tool.export, rs_tool.export, "tool exports must match");
    assert_eq!(
        py_tool.risk_level, rs_tool.risk_level,
        "tool risk levels must match"
    );
    assert_eq!(
        py_tool.parameters_schema, rs_tool.parameters_schema,
        "tool parameter schemas must match"
    );
}
