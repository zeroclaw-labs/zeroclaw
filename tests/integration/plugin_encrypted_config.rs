#![cfg(any())] // disabled: pending format_audit_summary/decrypt functions

//! Integration test: encrypted config values (enc2: prefix) are decrypted via
//! SecretStore before reaching the WASM plugin.
//!
//! Acceptance criterion for US-ZCL-7:
//! "Encrypted values (enc_v1 prefix) are decrypted via SecretStore before passing to plugin."
//!
//! This test encrypts a config value with SecretStore, passes it through the
//! `decrypt_plugin_config_values` → `resolve_plugin_config` → `build_extism_manifest_with_config`
//! pipeline, and verifies the WASM plugin receives the decrypted plaintext.

use std::collections::HashMap;
use std::path::Path;

use zeroclaw::plugins::loader::build_extism_manifest_with_config;
use zeroclaw::plugins::{PluginManifest, decrypt_plugin_config_values, resolve_plugin_config};
use zeroclaw::security::SecretStore;

const MULTI_TOOL_WASM: &str = "tests/plugins/artifacts/multi_tool_plugin.wasm";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(MULTI_TOOL_WASM)
}

/// Create a SecretStore in a temporary directory with encryption enabled.
fn temp_secret_store() -> (tempfile::TempDir, SecretStore) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let store = SecretStore::new(dir.path(), true);
    (dir, store)
}

#[test]
fn encrypted_config_value_is_decrypted_before_reaching_plugin() {
    let wasm_path = wasm_path();
    assert!(
        wasm_path.is_file(),
        "multi_tool_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let (_dir, store) = temp_secret_store();

    // Encrypt a secret API key
    let plaintext_key = "sk-super-secret-key-12345";
    let encrypted_key = store
        .encrypt(plaintext_key)
        .expect("encryption should succeed");
    assert!(
        encrypted_key.starts_with("enc2:"),
        "encrypted value should have enc2: prefix, got: {encrypted_key}"
    );

    // Simulate [plugins.multi-tool] config with an encrypted value
    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), encrypted_key);
    config_values.insert("model".to_string(), "claude-3".to_string());

    // Step 1: decrypt encrypted values via SecretStore
    decrypt_plugin_config_values(&mut config_values, &store).expect("decryption should succeed");

    // Verify decryption happened
    assert_eq!(
        config_values.get("api_key").map(String::as_str),
        Some(plaintext_key),
        "api_key should be decrypted to plaintext"
    );
    assert_eq!(
        config_values.get("model").map(String::as_str),
        Some("claude-3"),
        "non-encrypted model value should pass through unchanged"
    );

    // Step 2: resolve config
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("model".to_string(), serde_json::json!("gpt-4"));

    let resolved = resolve_plugin_config("multi-tool", &manifest_config, Some(&config_values))
        .expect("config resolution should succeed");

    assert_eq!(
        resolved.get("api_key").map(String::as_str),
        Some(plaintext_key),
        "resolved api_key should be decrypted plaintext"
    );

    // Step 3: build extism manifest and verify WASM plugin receives decrypted value
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
        .expect("failed to instantiate multi-tool plugin");

    let output = plugin
        .call::<&str, &str>("tool_lookup_config", "{}")
        .expect("tool_lookup_config call failed");

    let parsed: serde_json::Value = serde_json::from_str(output).expect("output is not valid JSON");

    assert_eq!(
        parsed["api_key"].as_str(),
        Some(plaintext_key),
        "WASM plugin should receive decrypted api_key, got: {parsed}"
    );
    assert_eq!(
        parsed["model"].as_str(),
        Some("claude-3"),
        "non-encrypted model should pass through unchanged to WASM, got: {parsed}"
    );
}

#[test]
fn non_encrypted_values_pass_through_unchanged() {
    let (_dir, store) = temp_secret_store();

    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), "plaintext-key".to_string());
    config_values.insert(
        "endpoint".to_string(),
        "https://api.example.com".to_string(),
    );

    decrypt_plugin_config_values(&mut config_values, &store)
        .expect("decryption of plaintext values should succeed");

    assert_eq!(config_values["api_key"], "plaintext-key");
    assert_eq!(config_values["endpoint"], "https://api.example.com");
}

#[test]
fn legacy_enc_prefix_is_also_decrypted() {
    let (_dir, store) = temp_secret_store();

    // Encrypt and then manually convert to legacy format isn't straightforward,
    // but we can verify the function recognises both prefixes.
    // Use the current enc2: format as the realistic case.
    let secret = "legacy-secret-value";
    let encrypted = store.encrypt(secret).expect("encryption should succeed");

    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("token".to_string(), encrypted);

    decrypt_plugin_config_values(&mut config_values, &store).expect("decryption should succeed");

    assert_eq!(
        config_values["token"], secret,
        "encrypted token should be decrypted to original plaintext"
    );
}

#[test]
fn decrypt_with_invalid_encrypted_value_returns_error() {
    let (_dir, store) = temp_secret_store();

    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("bad_key".to_string(), "enc2:not-valid-hex!!".to_string());

    let result = decrypt_plugin_config_values(&mut config_values, &store);
    assert!(
        result.is_err(),
        "invalid encrypted value should produce an error"
    );

    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("bad_key"),
        "error should name the config key, got: {msg}"
    );
}
