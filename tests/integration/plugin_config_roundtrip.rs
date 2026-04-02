//! Integration test: tool_lookup_config reads injected config values.
//!
//! Loads `multi_tool_plugin.wasm` with config `{api_key: "test-key", model: "test-model"}`,
//! calls `tool_lookup_config`, and asserts the returned JSON contains both values.
//!
//! The `config_toml_values_mapped_through_full_pipeline` test exercises the complete
//! `[plugins.<name>]` → `resolve_plugin_config` → `build_extism_manifest_with_config`
//! → WASM `config::get()` path (acceptance criterion for US-ZCL-7).

use std::collections::HashMap;
use std::path::Path;

use zeroclaw::plugins::loader::build_extism_manifest_with_config;
use zeroclaw::plugins::{resolve_plugin_config, PluginManifest};

const MULTI_TOOL_WASM: &str = "tests/plugins/artifacts/multi_tool_plugin.wasm";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MULTI_TOOL_WASM)
}

#[test]
fn tool_lookup_config_returns_injected_values() {
    let wasm_path = wasm_path();
    assert!(
        wasm_path.is_file(),
        "multi_tool_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let config = [("api_key", "test-key"), ("model", "test-model")];

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5))
        .with_config(config.into_iter());

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate multi-tool plugin");

    let output = plugin
        .call::<&str, &str>("tool_lookup_config", "{}")
        .expect("tool_lookup_config call failed");

    let parsed: serde_json::Value = serde_json::from_str(output).expect("output is not valid JSON");

    assert_eq!(
        parsed["api_key"].as_str(),
        Some("test-key"),
        "expected api_key='test-key', got: {parsed}"
    );
    assert_eq!(
        parsed["model"].as_str(),
        Some("test-model"),
        "expected model='test-model', got: {parsed}"
    );
}

#[test]
fn tool_lookup_config_without_config_returns_nulls() {
    let wasm_path = wasm_path();
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate multi-tool plugin");

    let output = plugin
        .call::<&str, &str>("tool_lookup_config", "{}")
        .expect("tool_lookup_config call failed");

    let parsed: serde_json::Value = serde_json::from_str(output).expect("output is not valid JSON");

    assert!(
        parsed["api_key"].is_null(),
        "api_key should be null when no config injected, got: {parsed}"
    );
    assert!(
        parsed["model"].is_null(),
        "model should be null when no config injected, got: {parsed}"
    );
}

/// Full pipeline test: simulates `[plugins.multi-tool]` config values flowing through
/// `resolve_plugin_config` → `build_extism_manifest_with_config` → WASM `config::get()`.
///
/// This is the acceptance-criterion test for US-ZCL-7:
/// "Config values from [plugins.<name>] are mapped to Extism config."
#[test]
fn config_toml_values_mapped_through_full_pipeline() {
    let wasm_path = wasm_path();
    assert!(
        wasm_path.is_file(),
        "multi_tool_plugin.wasm not found at {}",
        wasm_path.display()
    );

    // --- Manifest config: what the plugin declares it expects ---
    // api_key is required; model has a default of "gpt-4".
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("model".to_string(), serde_json::json!("gpt-4"));

    // --- Operator config: simulates [plugins.multi-tool] in config.toml ---
    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), "sk-from-config-toml".to_string());
    config_values.insert("model".to_string(), "claude-3".to_string());

    // Step 1: resolve config (as the runtime would)
    let resolved = resolve_plugin_config("multi-tool", &manifest_config, Some(&config_values))
        .expect("config resolution should succeed");

    assert_eq!(
        resolved.get("api_key").map(String::as_str),
        Some("sk-from-config-toml")
    );
    assert_eq!(resolved.get("model").map(String::as_str), Some("claude-3"));

    // Step 2: build extism manifest with resolved config
    let plugin_manifest = PluginManifest {
        name: "multi-tool".to_string(),
        version: "0.1.0".to_string(),
        description: None,
        author: None,
        wasm_path: wasm_path.to_string_lossy().to_string(),
        capabilities: vec![],
        permissions: vec![],
        allowed_hosts: vec![],
        allowed_paths: HashMap::new(),
        tools: vec![],
        config: manifest_config,
        wasi: true,
        timeout_ms: 5_000,
        signature: None,
        publisher_key: None,
        host_capabilities: Default::default(),
    };

    // Use "/" as plugin_dir since wasm_path is already absolute.
    let loader_manifest =
        build_extism_manifest_with_config(&plugin_manifest, Path::new("/"), resolved, None);

    // Step 3: instantiate the real WASM plugin with the built manifest
    let mut plugin = extism::Plugin::new(&loader_manifest.manifest, [], loader_manifest.wasi)
        .expect("failed to instantiate multi-tool plugin via loader");

    // Step 4: call tool_lookup_config and verify config values are readable inside WASM
    let output = plugin
        .call::<&str, &str>("tool_lookup_config", "{}")
        .expect("tool_lookup_config call failed");

    let parsed: serde_json::Value = serde_json::from_str(output).expect("output is not valid JSON");

    assert_eq!(
        parsed["api_key"].as_str(),
        Some("sk-from-config-toml"),
        "api_key from [plugins.multi-tool] should reach WASM, got: {parsed}"
    );
    assert_eq!(
        parsed["model"].as_str(),
        Some("claude-3"),
        "model override from [plugins.multi-tool] should reach WASM (not default 'gpt-4'), got: {parsed}"
    );
}

/// Verifies that manifest defaults are used when `[plugins.<name>]` omits a declared key.
#[test]
fn config_toml_defaults_reach_wasm_when_key_omitted() {
    let wasm_path = wasm_path();
    assert!(wasm_path.is_file());

    // Manifest declares api_key (required) and model (default "gpt-4")
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("model".to_string(), serde_json::json!("gpt-4"));

    // Operator supplies only api_key — model should fall back to manifest default
    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), "sk-only-key".to_string());

    let resolved = resolve_plugin_config("multi-tool", &manifest_config, Some(&config_values))
        .expect("config resolution should succeed");

    let plugin_manifest = PluginManifest {
        name: "multi-tool".to_string(),
        version: "0.1.0".to_string(),
        description: None,
        author: None,
        wasm_path: wasm_path.to_string_lossy().to_string(),
        capabilities: vec![],
        permissions: vec![],
        allowed_hosts: vec![],
        allowed_paths: HashMap::new(),
        tools: vec![],
        config: manifest_config,
        wasi: true,
        timeout_ms: 5_000,
        signature: None,
        publisher_key: None,
        host_capabilities: Default::default(),
    };

    let loader_manifest =
        build_extism_manifest_with_config(&plugin_manifest, Path::new("/"), resolved, None);

    let mut plugin = extism::Plugin::new(&loader_manifest.manifest, [], loader_manifest.wasi)
        .expect("failed to instantiate plugin");

    let output = plugin
        .call::<&str, &str>("tool_lookup_config", "{}")
        .expect("tool_lookup_config call failed");

    let parsed: serde_json::Value = serde_json::from_str(output).expect("output is not valid JSON");

    assert_eq!(
        parsed["api_key"].as_str(),
        Some("sk-only-key"),
        "api_key from config.toml should reach WASM"
    );
    assert_eq!(
        parsed["model"].as_str(),
        Some("gpt-4"),
        "model should fall back to manifest default 'gpt-4' when not in config.toml, got: {parsed}"
    );
}
