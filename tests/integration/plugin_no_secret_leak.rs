//! Integration test: sensitive values are never logged or included in diagnostics.
//!
//! Acceptance criterion for US-ZCL-7:
//! "Sensitive values are never logged or included in diagnostics."
//!
//! Verifies that:
//! - Error messages from config resolution never contain secret values
//! - Debug/Display representations of plugin errors don't leak secrets
//! - Successful config resolution doesn't expose values in error paths

use std::collections::HashMap;

use zeroclaw::plugins::resolve_plugin_config;

/// `MissingConfig` errors list key names but never include supplied config values.
#[test]
fn missing_config_error_does_not_leak_supplied_values() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("db_url".to_string(), serde_json::json!({"required": true}));

    // Supply api_key but not db_url — the error should mention db_url (the missing key)
    // but must NOT include the value of api_key.
    let secret_value = "sk-live-super-secret-key-abc123";
    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), secret_value.to_string());

    let err = resolve_plugin_config("my-plugin", &manifest_config, Some(&config_values))
        .expect_err("should fail with db_url missing");

    let display_msg = err.to_string();
    let debug_msg = format!("{err:?}");

    assert!(
        !display_msg.contains(secret_value),
        "MissingConfig Display must not include supplied secret value, got: {display_msg}"
    );
    assert!(
        !debug_msg.contains(secret_value),
        "MissingConfig Debug must not include supplied secret value, got: {debug_msg}"
    );
}

/// When multiple config values are supplied but a required key is missing,
/// none of the supplied values appear in any error representation.
#[test]
fn missing_config_error_does_not_leak_any_supplied_value() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("db_url".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert(
        "webhook_secret".to_string(),
        serde_json::json!({"required": true}),
    );

    let secrets = [
        ("api_key", "sk-live-super-secret-key-abc123"),
        ("db_url", "postgres://admin:p4ssw0rd@prod-db:5432/main"),
    ];

    let mut config_values: HashMap<String, String> = HashMap::new();
    for (k, v) in &secrets {
        config_values.insert(k.to_string(), v.to_string());
    }

    let err = resolve_plugin_config("my-plugin", &manifest_config, Some(&config_values))
        .expect_err("should fail with webhook_secret missing");

    let display_msg = err.to_string();
    let debug_msg = format!("{err:?}");

    for (key, value) in &secrets {
        assert!(
            !display_msg.contains(value),
            "MissingConfig Display must not leak value for '{key}', got: {display_msg}"
        );
        assert!(
            !debug_msg.contains(value),
            "MissingConfig Debug must not leak value for '{key}', got: {debug_msg}"
        );
    }
}

/// Error messages must never contain encrypted ciphertext blobs.
/// Even if a config key has an encrypted value, the error should only
/// reference the key name.
#[test]
fn missing_config_error_does_not_leak_encrypted_values() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("token".to_string(), serde_json::json!({"required": true}));

    let encrypted_value = "enc2:deadbeefcafebabe1234567890abcdef";
    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), encrypted_value.to_string());

    let err = resolve_plugin_config("my-plugin", &manifest_config, Some(&config_values))
        .expect_err("should fail with token missing");

    let display_msg = err.to_string();
    let debug_msg = format!("{err:?}");

    assert!(
        !display_msg.contains(encrypted_value),
        "error Display must not leak encrypted value, got: {display_msg}"
    );
    assert!(
        !display_msg.contains("deadbeefcafebabe"),
        "error Display must not leak ciphertext payload, got: {display_msg}"
    );
    assert!(
        !debug_msg.contains(encrypted_value),
        "error Debug must not leak encrypted value, got: {debug_msg}"
    );
}

/// Default values from the manifest should not appear in error messages
/// when other required keys are missing.
#[test]
fn missing_config_error_does_not_leak_default_values() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert(
        "model".to_string(),
        serde_json::json!({"default": "gpt-4-internal-staging"}),
    );
    manifest_config.insert(
        "endpoint".to_string(),
        serde_json::json!("https://internal.corp.example.com/api"),
    );

    let err = resolve_plugin_config("my-plugin", &manifest_config, None)
        .expect_err("should fail with api_key missing");

    let display_msg = err.to_string();

    // Default values should not appear in the error (they're not secrets per se,
    // but error messages should only contain diagnostic info about what's wrong).
    assert!(
        !display_msg.contains("gpt-4-internal-staging"),
        "error should not include default values, got: {display_msg}"
    );
    assert!(
        !display_msg.contains("https://internal.corp.example.com"),
        "error should not include default endpoint, got: {display_msg}"
    );
}

/// Successful config resolution returns only key-value pairs, not error
/// diagnostics that might leak values. This verifies the happy path
/// keeps secrets contained in the returned map only.
#[test]
fn successful_resolution_contains_only_expected_keys() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("api_key".to_string(), serde_json::json!({"required": true}));
    manifest_config.insert("model".to_string(), serde_json::json!("gpt-4"));

    let secret = "sk-live-extremely-secret-key-xyz";
    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), secret.to_string());

    let resolved = resolve_plugin_config("my-plugin", &manifest_config, Some(&config_values))
        .expect("should succeed when all required keys are supplied");

    // Verify that secrets are in the resolved config (they need to be passed to WASM)
    assert_eq!(resolved.get("api_key").map(String::as_str), Some(secret));
    assert_eq!(resolved.get("model").map(String::as_str), Some("gpt-4"));

    // But the resolved map should not contain any extra keys or leak info
    assert_eq!(
        resolved.len(),
        2,
        "resolved config should have exactly the expected keys"
    );
}
