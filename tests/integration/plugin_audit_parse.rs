//! Integration test: `zeroclaw plugin audit <path>` parses plugin.toml without installing.
//!
//! Verifies the acceptance criterion:
//! > zeroclaw plugin audit <path> parses plugin.toml without installing
//!
//! We exercise the same code path used by the CLI handler: read plugin.toml
//! from disk, parse it with `PluginManifest::parse`, and confirm the manifest
//! is correct — all without instantiating the WASM runtime or touching the
//! plugin install directory.

use std::path::Path;

use zeroclaw::plugins::PluginManifest;

/// Locate the test plugins directory.
fn test_plugins_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins")
}

/// Audit should locate and parse `plugin.toml` from a plugin directory.
#[test]
fn audit_parses_plugin_toml_from_directory() {
    let plugin_dir = test_plugins_dir().join("echo-plugin");
    let manifest_path = plugin_dir.join("plugin.toml");
    assert!(
        manifest_path.is_file(),
        "echo-plugin/plugin.toml should exist at {}",
        manifest_path.display()
    );

    // Replicate the CLI handler logic: read and parse, no install.
    let content = std::fs::read_to_string(&manifest_path)
        .expect("should read plugin.toml");
    let manifest = PluginManifest::parse(&content)
        .expect("should parse plugin.toml without error");

    assert_eq!(manifest.name, "echo-plugin");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(
        manifest.description.as_deref(),
        Some("Echoes JSON input back as output — used for round-trip integration tests.")
    );
    assert!(!manifest.tools.is_empty(), "echo-plugin should declare at least one tool");
    assert_eq!(manifest.tools[0].name, "tool_echo");
}

/// Audit should parse a manifest given a direct file path (not a directory).
#[test]
fn audit_parses_plugin_toml_by_file_path() {
    let manifest_path = test_plugins_dir().join("echo-plugin/plugin.toml");

    let content = std::fs::read_to_string(&manifest_path)
        .expect("should read plugin.toml by file path");
    let manifest = PluginManifest::parse(&content)
        .expect("should parse plugin.toml from direct file path");

    assert_eq!(manifest.name, "echo-plugin");
}

/// Parsing is purely in-memory — no WASM is loaded and no install directory is touched.
/// We verify this by parsing a manifest that references a non-existent .wasm file;
/// if parsing tried to load the WASM, it would fail.
#[test]
fn audit_does_not_install_or_load_wasm() {
    let toml_str = r#"
[plugin]
name = "phantom-plugin"
version = "1.0.0"
description = "References a WASM file that does not exist on disk"
wasm_path = "nonexistent_phantom_plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "phantom_tool"
description = "A tool that can never run"
export = "phantom_tool"
risk_level = "high"
parameters_schema = { type = "object" }
"#;

    // Parsing must succeed even though the WASM binary doesn't exist,
    // proving that audit only reads the manifest — it does not install.
    let manifest = PluginManifest::parse(toml_str)
        .expect("parse should succeed without loading WASM");

    assert_eq!(manifest.name, "phantom-plugin");
    assert_eq!(manifest.wasm_path, "nonexistent_phantom_plugin.wasm");
    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].name, "phantom_tool");
}

/// The CLI handler auto-detects plugin.toml inside a directory.
/// Replicate that path-resolution logic and confirm it finds the right file.
#[test]
fn audit_path_resolution_prefers_plugin_toml_in_directory() {
    let plugin_dir = test_plugins_dir().join("echo-plugin");

    // Replicate the CLI's path resolution logic.
    let p = Path::new(&plugin_dir);
    let resolved = if p.is_file() {
        p.to_path_buf()
    } else {
        let candidate = p.join("plugin.toml");
        if candidate.exists() {
            candidate
        } else {
            p.join("manifest.toml")
        }
    };

    assert!(
        resolved.ends_with("plugin.toml"),
        "should resolve to plugin.toml, got: {}",
        resolved.display()
    );
    assert!(resolved.is_file(), "resolved path should exist");

    let content = std::fs::read_to_string(&resolved).expect("should read resolved manifest");
    let manifest = PluginManifest::parse(&content).expect("should parse resolved manifest");
    assert_eq!(manifest.name, "echo-plugin");
}
