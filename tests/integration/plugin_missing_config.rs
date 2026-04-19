#![cfg(feature = "plugins-wasm")]

//! Integration test: missing required config keys produce actionable error messages.
//!
//! Acceptance criterion for US-ZCL-7:
//! "Missing required config keys produce actionable error messages."
//!
//! Verifies that `resolve_plugin_config` returns `PluginError::MissingConfig`
//! with a human-readable message that names the plugin and all missing keys.

use std::collections::HashMap;

use zeroclaw::plugins::resolve_plugin_config;

/// When all required keys are missing, the error names the plugin and lists every key.
#[test]
fn missing_all_required_keys_names_plugin_and_keys() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("db_url".to_string(), serde_json::json!({"required": true}));

    // No config values supplied at all.
    let err = resolve_plugin_config("weather-plugin", &manifest_config, None)
        .expect_err("should fail when required keys are missing");

    let msg = err.to_string();
    assert!(
        msg.contains("weather-plugin"),
        "error should name the plugin, got: {msg}"
    );
    assert!(
        msg.contains("api_key"),
        "error should list missing key 'api_key', got: {msg}"
    );
    assert!(
        msg.contains("db_url"),
        "error should list missing key 'db_url', got: {msg}"
    );
}

/// When only some required keys are supplied, only the missing ones appear in the error.
#[test]
fn partial_config_lists_only_unsupplied_required_keys() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("db_url".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("model".to_string(), serde_json::json!("gpt-4"));

    // Supply only api_key — db_url is still missing.
    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), "sk-supplied".to_string());

    let err = resolve_plugin_config("weather-plugin", &manifest_config, Some(&config_values))
        .expect_err("should fail when db_url is missing");

    let msg = err.to_string();
    assert!(
        msg.contains("db_url"),
        "error should list missing key 'db_url', got: {msg}"
    );
    assert!(
        !msg.contains("api_key"),
        "error should NOT list supplied key 'api_key', got: {msg}"
    );
    assert!(
        !msg.contains("model"),
        "error should NOT list key with default 'model', got: {msg}"
    );
}

/// When all required keys are supplied, resolution succeeds even if other keys have defaults.
#[test]
fn all_required_keys_supplied_succeeds() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("model".to_string(), serde_json::json!("gpt-4"));

    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), "sk-valid".to_string());

    let resolved = resolve_plugin_config("weather-plugin", &manifest_config, Some(&config_values))
        .expect("should succeed when all required keys are supplied");

    assert_eq!(
        resolved.get("api_key").map(String::as_str),
        Some("sk-valid")
    );
    assert_eq!(resolved.get("model").map(String::as_str), Some("gpt-4"));
}

/// An empty config section (Some(&empty)) still triggers missing-config for required keys.
#[test]
fn empty_config_section_triggers_missing_required() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("token".to_string(), serde_json::json!({"required": true}));

    let empty = HashMap::new();
    let err = resolve_plugin_config("slack-bridge", &manifest_config, Some(&empty))
        .expect_err("should fail with empty config section");

    let msg = err.to_string();
    assert!(
        msg.contains("slack-bridge"),
        "error should name the plugin, got: {msg}"
    );
    assert!(
        msg.contains("token"),
        "error should list missing key 'token', got: {msg}"
    );
}

/// Multiple missing keys are sorted alphabetically for deterministic, scannable output.
#[test]
fn missing_keys_are_sorted_alphabetically() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    // Insert in reverse-alpha order to verify sorting.
    manifest_config.insert("z_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("a_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("m_key".to_string(), serde_json::json!({"required": true}));

    let err = resolve_plugin_config("sort-test", &manifest_config, None)
        .expect_err("should fail with all keys missing");

    let msg = err.to_string();
    // The keys should appear in order: a_key, m_key, z_key
    let a_pos = msg.find("a_key").expect("a_key missing from error");
    let m_pos = msg.find("m_key").expect("m_key missing from error");
    let z_pos = msg.find("z_key").expect("z_key missing from error");
    assert!(
        a_pos < m_pos && m_pos < z_pos,
        "missing keys should be sorted alphabetically, got: {msg}"
    );
}
