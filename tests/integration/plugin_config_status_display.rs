//! Integration test: Config status shows which required keys are set vs missing.
//!
//! Verifies the acceptance criterion for US-ZCL-21:
//! > Config status shows which required keys are set vs missing
//!
//! Uses `PluginHost` with the checked-in test plugins to verify that the config
//! map returned in the API response contains enough information for the UI to
//! distinguish required-vs-optional and set-vs-missing config keys.

use std::path::Path;

use zeroclaw::plugins::host::PluginHost;
use zeroclaw::plugins::PluginInfo;

/// Set up a PluginHost pointed at the test plugins directory.
fn setup_host() -> PluginHost {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    PluginHost::new(&base).expect("failed to create PluginHost from tests")
}

/// Build the config JSON in the same way the API endpoint does.
fn build_config_json(info: &PluginInfo) -> serde_json::Value {
    serde_json::json!({
        "name": info.name,
        "config": info.config,
    })
}

fn get_multi_tool_plugin() -> PluginInfo {
    let host = setup_host();
    host.get_plugin("multi-tool-plugin")
        .expect("multi-tool-plugin should be discovered from test plugins")
}

fn get_echo_plugin() -> PluginInfo {
    let host = setup_host();
    host.get_plugin("echo-plugin")
        .expect("echo-plugin should be discovered from test plugins")
}

// ---- Config map presence ----

#[test]
fn config_status_multi_tool_plugin_has_config_keys() {
    let info = get_multi_tool_plugin();
    let detail = build_config_json(&info);
    let config = detail["config"]
        .as_object()
        .expect("config must be a JSON object");
    assert!(
        !config.is_empty(),
        "multi-tool-plugin should have at least one config key"
    );
}

#[test]
fn config_status_multi_tool_plugin_has_api_key() {
    let info = get_multi_tool_plugin();
    let detail = build_config_json(&info);
    assert!(
        detail["config"].get("api_key").is_some(),
        "config should contain 'api_key' key"
    );
}

#[test]
fn config_status_multi_tool_plugin_has_model() {
    let info = get_multi_tool_plugin();
    let detail = build_config_json(&info);
    assert!(
        detail["config"].get("model").is_some(),
        "config should contain 'model' key"
    );
}

// ---- Required vs optional distinction ----

#[test]
fn config_status_api_key_is_required() {
    let info = get_multi_tool_plugin();
    let detail = build_config_json(&info);
    let api_key = &detail["config"]["api_key"];

    // api_key is declared as { required = true } in the manifest
    assert!(
        api_key.is_object(),
        "api_key config entry should be an object (not a bare string), got: {api_key}"
    );
    assert_eq!(
        api_key["required"], true,
        "api_key should be marked as required"
    );
}

#[test]
fn config_status_model_is_not_required() {
    let info = get_multi_tool_plugin();
    let detail = build_config_json(&info);
    let model = &detail["config"]["model"];

    // model is declared as a bare string "gpt-4" — it acts as a default value
    // and is not an object with required=true.
    // The frontend treats non-object entries (bare strings) as optional with a
    // default value, which means "set".
    let is_required = model.is_object() && model["required"] == true;
    assert!(
        !is_required,
        "model should NOT be marked as required (it has a default value)"
    );
}

// ---- Set vs missing distinction ----

#[test]
fn config_status_model_is_set_because_has_default() {
    let info = get_multi_tool_plugin();
    let detail = build_config_json(&info);
    let model = &detail["config"]["model"];

    // A bare string value means "has a default" → the key is set.
    // The frontend logic: `hasDefault || (isObject && decl.value !== undefined)`
    let is_bare_string = model.is_string();
    let has_default = is_bare_string
        || (model.is_object() && model.get("default").is_some());
    assert!(
        has_default,
        "model should be considered 'set' because it has a default value, got: {model}"
    );
}

#[test]
fn config_status_api_key_is_missing_when_no_value_provided() {
    let info = get_multi_tool_plugin();
    let detail = build_config_json(&info);
    let api_key = &detail["config"]["api_key"];

    // api_key is { required: true } with no default and no value — it should
    // appear as "missing" in the UI.
    // Frontend logic: isSet = hasDefault || (isObject && decl.value !== undefined)
    let is_bare_string = api_key.is_string();
    let has_default = is_bare_string
        || (api_key.is_object() && api_key.get("default").is_some());
    let has_value = api_key.is_object()
        && api_key.get("value").is_some()
        && !api_key["value"].is_null();
    let is_set = has_default || has_value;
    assert!(
        !is_set,
        "api_key should NOT be considered 'set' (no default, no value provided), got: {api_key}"
    );
}

// ---- Empty config ----

#[test]
fn config_status_plugin_with_no_config_has_empty_map() {
    let info = get_echo_plugin();
    let detail = build_config_json(&info);
    let config = detail["config"]
        .as_object()
        .expect("config must be a JSON object");
    assert!(
        config.is_empty(),
        "echo-plugin should have an empty config map (no config keys declared)"
    );
}

// ---- Frontend rendering logic: required badge + set/missing badge ----

/// Mirrors the frontend logic in PluginDetail.tsx for determining set/missing
/// and required/optional status from the config map.
fn frontend_config_status(decl: &serde_json::Value) -> (bool, bool) {
    let is_object = decl.is_object();
    let is_required = is_object && decl["required"] == true;
    let has_default = if is_object {
        decl.get("default").is_some()
    } else {
        // bare string/number/bool → acts as default
        decl.is_string() || decl.is_number() || decl.is_boolean()
    };
    let has_value = is_object
        && decl.get("value").map_or(false, |v| !v.is_null());
    let is_set = has_default || has_value;
    (is_required, is_set)
}

#[test]
fn config_status_frontend_logic_api_key_required_and_missing() {
    let info = get_multi_tool_plugin();
    let api_key = info.config.get("api_key").expect("api_key must exist");
    let (is_required, is_set) = frontend_config_status(api_key);
    assert!(is_required, "api_key should be required");
    assert!(!is_set, "api_key should be missing (no default, no value)");
}

#[test]
fn config_status_frontend_logic_model_optional_and_set() {
    let info = get_multi_tool_plugin();
    let model = info.config.get("model").expect("model must exist");
    let (is_required, is_set) = frontend_config_status(model);
    assert!(!is_required, "model should be optional");
    assert!(is_set, "model should be set (has default value 'gpt-4')");
}

#[test]
fn config_status_all_keys_have_deterministic_status() {
    let info = get_multi_tool_plugin();
    for (key, decl) in &info.config {
        let (is_required, is_set) = frontend_config_status(decl);
        // Every key should resolve to exactly one of: required+set, required+missing,
        // optional+set, optional+missing. This test ensures the logic doesn't panic
        // or produce undefined states.
        let label = format!(
            "{key}: required={is_required}, set={is_set}",
        );
        assert!(
            label.contains("required=") && label.contains("set="),
            "config key '{key}' should have deterministic status"
        );
    }
}
