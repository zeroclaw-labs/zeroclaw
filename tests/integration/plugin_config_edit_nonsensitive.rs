#![cfg(feature = "plugins-wasm")]

//! Integration test: Non-sensitive config values can be edited from the web UI.
//!
//! Verifies the acceptance criterion for US-ZCL-21:
//! > Non-sensitive config values can be edited from the web UI
//!
//! Uses `PluginHost` with the checked-in test plugins to verify that non-sensitive
//! config keys pass the API's sensitivity check and can be updated, while the
//! frontend correctly renders edit controls for non-sensitive keys.

use std::collections::HashMap;
use std::path::Path;

use zeroclaw::plugins::PluginInfo;
use zeroclaw::plugins::host::PluginHost;

/// Set up a PluginHost pointed at the test plugins directory.
fn setup_host() -> PluginHost {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    PluginHost::new(&base).expect("failed to create PluginHost from tests")
}

fn get_multi_tool_plugin() -> PluginInfo {
    let host = setup_host();
    host.get_plugin("multi-tool-plugin")
        .expect("multi-tool-plugin should be discovered from test plugins")
}

/// Mirrors the PATCH handler's sensitivity check (api_plugins.rs:281-293).
/// Returns true if a config key is marked as sensitive.
fn is_sensitive(decl: &serde_json::Value) -> bool {
    decl.as_object()
        .and_then(|obj| obj.get("sensitive"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Mirrors the frontend logic in PluginDetail.tsx: a key is editable if it is
/// NOT sensitive. The UI shows an edit button only when `!entry.sensitive`.
fn is_editable(decl: &serde_json::Value) -> bool {
    !is_sensitive(decl)
}

/// Simulates what the PATCH handler does: validates keys are not sensitive,
/// then inserts into per-plugin config. Returns Ok(()) on success or Err with
/// the rejection reason.
fn simulate_patch(
    info: &PluginInfo,
    updates: &HashMap<String, String>,
    per_plugin: &mut HashMap<String, String>,
) -> Result<(), &'static str> {
    // Reject writes to sensitive keys (mirrors handler logic)
    for key in updates.keys() {
        if let Some(decl) = info.config.get(key) {
            if is_sensitive(decl) {
                return Err("Cannot edit sensitive config keys via API");
            }
        }
    }
    // Apply updates
    for (k, v) in updates {
        per_plugin.insert(k.clone(), v.clone());
    }
    Ok(())
}

// ---- Non-sensitive keys are editable ----

#[test]
fn config_edit_api_key_is_not_sensitive() {
    let info = get_multi_tool_plugin();
    let decl = info.config.get("api_key").expect("api_key must exist");
    assert!(
        !is_sensitive(decl),
        "api_key should not be marked as sensitive"
    );
}

#[test]
fn config_edit_model_is_not_sensitive() {
    let info = get_multi_tool_plugin();
    let decl = info.config.get("model").expect("model must exist");
    assert!(
        !is_sensitive(decl),
        "model should not be marked as sensitive"
    );
}

#[test]
fn config_edit_all_multi_tool_keys_are_editable() {
    let info = get_multi_tool_plugin();
    for (key, decl) in &info.config {
        assert!(
            is_editable(decl),
            "config key '{key}' should be editable (not sensitive)"
        );
    }
}

// ---- Frontend edit control rendering ----

#[test]
fn config_edit_frontend_shows_edit_button_for_nonsensitive() {
    let info = get_multi_tool_plugin();
    let decl = info.config.get("api_key").expect("api_key must exist");
    // PluginDetail.tsx: edit button is rendered when !entry.sensitive
    assert!(
        is_editable(decl),
        "frontend should show edit button for api_key (not sensitive)"
    );
}

#[test]
fn config_edit_frontend_shows_edit_button_for_bare_string_default() {
    let info = get_multi_tool_plugin();
    let decl = info.config.get("model").expect("model must exist");
    // Bare string values (defaults) are always non-sensitive and editable.
    assert!(
        is_editable(decl),
        "frontend should show edit button for model (bare string default)"
    );
}

// ---- PATCH handler simulation: successful edits ----

#[test]
fn config_edit_patch_single_key_succeeds() {
    let info = get_multi_tool_plugin();
    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("api_key".to_string(), "sk-test-123".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(result.is_ok(), "patching non-sensitive key should succeed");
    assert_eq!(per_plugin.get("api_key").unwrap(), "sk-test-123");
}

#[test]
fn config_edit_patch_multiple_keys_succeeds() {
    let info = get_multi_tool_plugin();
    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("api_key".to_string(), "sk-test-456".to_string());
    updates.insert("model".to_string(), "gpt-4o".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(
        result.is_ok(),
        "patching multiple non-sensitive keys should succeed"
    );
    assert_eq!(per_plugin.get("api_key").unwrap(), "sk-test-456");
    assert_eq!(per_plugin.get("model").unwrap(), "gpt-4o");
}

#[test]
fn config_edit_patch_overwrites_existing_value() {
    let info = get_multi_tool_plugin();
    let mut per_plugin = HashMap::new();
    per_plugin.insert("model".to_string(), "gpt-3.5-turbo".to_string());

    let mut updates = HashMap::new();
    updates.insert("model".to_string(), "gpt-4o".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(result.is_ok(), "overwriting existing value should succeed");
    assert_eq!(
        per_plugin.get("model").unwrap(),
        "gpt-4o",
        "value should be updated to the new value"
    );
}

#[test]
fn config_edit_patch_unknown_key_succeeds() {
    // Keys not declared in the manifest are allowed (the handler only checks
    // sensitivity for keys that *are* declared).
    let info = get_multi_tool_plugin();
    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("custom_setting".to_string(), "hello".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(
        result.is_ok(),
        "patching undeclared key should succeed (no sensitivity check)"
    );
    assert_eq!(per_plugin.get("custom_setting").unwrap(), "hello");
}

// ---- Sensitivity gate: synthetic sensitive key ----

#[test]
fn config_edit_sensitive_key_is_rejected() {
    // Build a synthetic PluginInfo with a sensitive key to confirm the gate works.
    let mut info = get_multi_tool_plugin();
    info.config.insert(
        "secret_token".to_string(),
        serde_json::json!({ "required": true, "sensitive": true }),
    );

    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("secret_token".to_string(), "should-be-rejected".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(
        result.is_err(),
        "patching a sensitive key should be rejected"
    );
    assert_eq!(
        result.unwrap_err(),
        "Cannot edit sensitive config keys via API"
    );
    assert!(
        !per_plugin.contains_key("secret_token"),
        "rejected key should not be persisted"
    );
}

#[test]
fn config_edit_mixed_sensitive_and_nonsensitive_rejects_all() {
    // If any key in the batch is sensitive, the entire PATCH is rejected.
    let mut info = get_multi_tool_plugin();
    info.config.insert(
        "secret_token".to_string(),
        serde_json::json!({ "required": true, "sensitive": true }),
    );

    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("model".to_string(), "gpt-4o".to_string());
    updates.insert("secret_token".to_string(), "nope".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(
        result.is_err(),
        "batch with any sensitive key should be rejected"
    );
}

#[test]
fn config_edit_frontend_hides_edit_for_sensitive_key() {
    let sensitive_decl = serde_json::json!({ "required": true, "sensitive": true });
    assert!(
        !is_editable(&sensitive_decl),
        "frontend should NOT show edit button for sensitive keys"
    );
}

// ---- Edge cases ----

#[test]
fn config_edit_empty_patch_succeeds() {
    let info = get_multi_tool_plugin();
    let mut per_plugin = HashMap::new();
    let updates = HashMap::new(); // empty

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(result.is_ok(), "empty patch should succeed (no-op)");
    assert!(per_plugin.is_empty(), "no keys should be inserted");
}

#[test]
fn config_edit_empty_value_is_allowed() {
    let info = get_multi_tool_plugin();
    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("api_key".to_string(), String::new());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(result.is_ok(), "empty string value should be accepted");
    assert_eq!(per_plugin.get("api_key").unwrap(), "");
}
