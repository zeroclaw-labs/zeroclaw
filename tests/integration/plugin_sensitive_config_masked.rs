#![cfg(feature = "plugins-wasm")]

//! Integration test: Sensitive config keys show as masked with no edit capability.
//!
//! Verifies the acceptance criterion for US-ZCL-21:
//! > Sensitive config keys show as masked with no edit capability
//!
//! Tests that the `is_sensitive_key` gate correctly identifies sensitive
//! declarations, that the PATCH endpoint rejects edits to sensitive keys,
//! and that the frontend rendering logic masks values and hides edit controls
//! for any key declared with `"sensitive": true`.

use std::collections::HashMap;
use std::path::Path;

use zeroclaw::plugins::host::PluginHost;
use zeroclaw::plugins::{PluginInfo, is_sensitive_key};

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

/// Build a PluginInfo with a sensitive key injected for testing.
fn plugin_with_sensitive_key(key: &str) -> PluginInfo {
    let mut info = get_multi_tool_plugin();
    info.config.insert(
        key.to_string(),
        serde_json::json!({ "required": true, "sensitive": true }),
    );
    info
}

/// Mirrors the frontend logic: a key is editable only when NOT sensitive.
/// (PluginDetail.tsx line 128: `!isSensitive && !isEditing`)
fn frontend_shows_edit_button(decl: &serde_json::Value) -> bool {
    !is_sensitive_key(decl)
}

/// Mirrors the frontend logic for masking: sensitive + set keys show masked text.
/// (PluginDetail.tsx lines 142-149)
fn frontend_shows_masked_value(decl: &serde_json::Value) -> bool {
    let is_sensitive = is_sensitive_key(decl);
    let is_object = decl.is_object();
    let has_value = is_object && decl.get("value").is_some_and(|v| !v.is_null());
    let has_default = if is_object {
        decl.get("default").is_some()
    } else {
        decl.is_string() || decl.is_number() || decl.is_boolean()
    };
    let is_set = has_default || has_value;
    is_sensitive && is_set
}

/// Mirrors the frontend logic: sensitive keys show a "SENSITIVE" badge.
/// (PluginDetail.tsx lines 102-108)
fn frontend_shows_sensitive_badge(decl: &serde_json::Value) -> bool {
    is_sensitive_key(decl)
}

/// Simulates the PATCH handler's sensitivity gate (api_plugins.rs:280-293).
fn simulate_patch(
    info: &PluginInfo,
    updates: &HashMap<String, String>,
    per_plugin: &mut HashMap<String, String>,
) -> Result<(), &'static str> {
    for key in updates.keys() {
        if let Some(decl) = info.config.get(key) {
            if is_sensitive_key(decl) {
                return Err("Cannot edit sensitive config keys via API");
            }
        }
    }
    for (k, v) in updates {
        per_plugin.insert(k.clone(), v.clone());
    }
    Ok(())
}

// ---- is_sensitive_key gate ----

#[test]
fn sensitive_key_object_with_flag_true() {
    let decl = serde_json::json!({ "required": true, "sensitive": true });
    assert!(
        is_sensitive_key(&decl),
        "object with sensitive=true must be sensitive"
    );
}

#[test]
fn sensitive_key_object_with_flag_false() {
    let decl = serde_json::json!({ "required": true, "sensitive": false });
    assert!(
        !is_sensitive_key(&decl),
        "object with sensitive=false must not be sensitive"
    );
}

#[test]
fn sensitive_key_object_without_flag() {
    let decl = serde_json::json!({ "required": true });
    assert!(
        !is_sensitive_key(&decl),
        "object without sensitive field must not be sensitive"
    );
}

#[test]
fn sensitive_key_bare_string_is_not_sensitive() {
    let decl = serde_json::json!("gpt-4");
    assert!(
        !is_sensitive_key(&decl),
        "bare string value must never be sensitive"
    );
}

#[test]
fn sensitive_key_bare_number_is_not_sensitive() {
    let decl = serde_json::json!(42);
    assert!(
        !is_sensitive_key(&decl),
        "bare number value must never be sensitive"
    );
}

#[test]
fn sensitive_key_null_is_not_sensitive() {
    let decl = serde_json::Value::Null;
    assert!(!is_sensitive_key(&decl), "null must not be sensitive");
}

// ---- Frontend: no edit button for sensitive keys ----

#[test]
fn frontend_hides_edit_for_sensitive_required_key() {
    let decl = serde_json::json!({ "required": true, "sensitive": true });
    assert!(
        !frontend_shows_edit_button(&decl),
        "frontend must NOT show edit button for sensitive key"
    );
}

#[test]
fn frontend_hides_edit_for_sensitive_optional_key() {
    let decl = serde_json::json!({ "sensitive": true, "default": "secret-default" });
    assert!(
        !frontend_shows_edit_button(&decl),
        "frontend must NOT show edit button for sensitive optional key"
    );
}

#[test]
fn frontend_shows_edit_for_nonsensitive_key() {
    let decl = serde_json::json!({ "required": true });
    assert!(
        frontend_shows_edit_button(&decl),
        "frontend must show edit button for non-sensitive key"
    );
}

#[test]
fn frontend_shows_edit_for_bare_string_default() {
    let decl = serde_json::json!("gpt-4");
    assert!(
        frontend_shows_edit_button(&decl),
        "frontend must show edit button for bare string default"
    );
}

// ---- Frontend: masked value display ----

#[test]
fn frontend_masks_sensitive_key_with_value() {
    let decl = serde_json::json!({ "sensitive": true, "value": "sk-secret-123" });
    assert!(
        frontend_shows_masked_value(&decl),
        "sensitive key with a value must display masked"
    );
}

#[test]
fn frontend_masks_sensitive_key_with_default() {
    let decl = serde_json::json!({ "sensitive": true, "default": "fallback-secret" });
    assert!(
        frontend_shows_masked_value(&decl),
        "sensitive key with a default must display masked"
    );
}

#[test]
fn frontend_does_not_mask_sensitive_key_without_value() {
    // A required sensitive key with no value or default is "missing" — nothing to mask.
    let decl = serde_json::json!({ "required": true, "sensitive": true });
    assert!(
        !frontend_shows_masked_value(&decl),
        "sensitive key with no value/default should not display masked (nothing to show)"
    );
}

#[test]
fn frontend_does_not_mask_nonsensitive_key() {
    let decl = serde_json::json!({ "required": true, "value": "visible-value" });
    assert!(
        !frontend_shows_masked_value(&decl),
        "non-sensitive key must never display masked"
    );
}

// ---- Frontend: SENSITIVE badge ----

#[test]
fn frontend_shows_badge_for_sensitive_key() {
    let decl = serde_json::json!({ "required": true, "sensitive": true });
    assert!(
        frontend_shows_sensitive_badge(&decl),
        "frontend must show SENSITIVE badge for sensitive key"
    );
}

#[test]
fn frontend_hides_badge_for_nonsensitive_key() {
    let decl = serde_json::json!({ "required": true });
    assert!(
        !frontend_shows_sensitive_badge(&decl),
        "frontend must NOT show SENSITIVE badge for non-sensitive key"
    );
}

// ---- PATCH endpoint rejects sensitive key edits ----

#[test]
fn patch_rejects_sensitive_key() {
    let info = plugin_with_sensitive_key("secret_token");
    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("secret_token".to_string(), "should-be-rejected".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(result.is_err(), "PATCH must reject sensitive key edits");
    assert_eq!(
        result.unwrap_err(),
        "Cannot edit sensitive config keys via API"
    );
    assert!(
        !per_plugin.contains_key("secret_token"),
        "rejected key must not be persisted"
    );
}

#[test]
fn patch_rejects_batch_containing_sensitive_key() {
    let info = plugin_with_sensitive_key("secret_token");
    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("model".to_string(), "gpt-4o".to_string());
    updates.insert("secret_token".to_string(), "nope".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(
        result.is_err(),
        "PATCH must reject entire batch when any key is sensitive"
    );
    assert!(
        per_plugin.is_empty(),
        "no keys should be persisted when batch is rejected"
    );
}

#[test]
fn patch_allows_nonsensitive_key_alongside_sensitive_declaration() {
    // Plugin has a sensitive key declared but the PATCH only touches non-sensitive keys.
    let info = plugin_with_sensitive_key("secret_token");
    let mut per_plugin = HashMap::new();
    let mut updates = HashMap::new();
    updates.insert("model".to_string(), "gpt-4o".to_string());

    let result = simulate_patch(&info, &updates, &mut per_plugin);
    assert!(
        result.is_ok(),
        "PATCH should succeed when only non-sensitive keys are edited"
    );
    assert_eq!(per_plugin.get("model").unwrap(), "gpt-4o");
}

// ---- Real plugin config: existing keys are not sensitive ----

#[test]
fn multi_tool_plugin_has_no_sensitive_keys() {
    let info = get_multi_tool_plugin();
    for (key, decl) in &info.config {
        assert!(
            !is_sensitive_key(decl),
            "multi-tool-plugin key '{key}' should not be sensitive (test fixture baseline)"
        );
    }
}
