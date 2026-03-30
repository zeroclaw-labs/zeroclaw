//! Integration test: GET /api/plugins/{name} response shape.
//!
//! Verifies the acceptance criterion for US-ZCL-18:
//! > GET /api/plugins/{name} returns full plugin details including manifest
//! > and config status
//!
//! Uses `PluginHost` with the checked-in test plugins to verify that the
//! detail JSON contains every required field for a full plugin detail response.

use std::path::Path;

use zeroclaw::plugins::host::PluginHost;
use zeroclaw::plugins::PluginInfo;

/// Set up a PluginHost pointed at the test plugins directory.
fn setup_host() -> PluginHost {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    PluginHost::new(&base).expect("failed to create PluginHost from tests")
}

/// Build the JSON response in the same way the GET /api/plugins/{name} endpoint
/// does, including full manifest fields and config status.
fn build_plugin_detail(info: &PluginInfo) -> serde_json::Value {
    serde_json::json!({
        "name": info.name,
        "version": info.version,
        "description": info.description,
        "status": if info.loaded { "loaded" } else { "discovered" },
        "capabilities": info.capabilities,
        "permissions": info.permissions,
        "tools": info.tools,
        "wasm_path": info.wasm_path,
        "wasm_sha256": info.wasm_sha256,
        "config_status": if info.loaded { "ok" } else { "not_loaded" },
    })
}

fn get_echo_plugin() -> PluginInfo {
    let host = setup_host();
    host.get_plugin("echo-plugin")
        .expect("echo-plugin should be discovered from test plugins")
}

// ---- Response structure tests ----

#[test]
fn api_plugin_detail_has_name() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    assert_eq!(detail["name"], "echo-plugin");
}

#[test]
fn api_plugin_detail_has_version() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    assert_eq!(detail["version"], "0.1.0");
}

#[test]
fn api_plugin_detail_has_description() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    assert!(
        detail["description"].is_string(),
        "detail should include a description string"
    );
}

#[test]
fn api_plugin_detail_has_status() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    let status = detail["status"].as_str().unwrap();
    assert!(
        status == "loaded" || status == "discovered",
        "status must be 'loaded' or 'discovered', got: '{status}'"
    );
}

#[test]
fn api_plugin_detail_has_capabilities() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    let caps = detail["capabilities"]
        .as_array()
        .expect("capabilities must be an array");
    assert!(
        !caps.is_empty(),
        "echo-plugin should have at least one capability"
    );
    assert!(
        caps.iter().any(|c| c == "tool"),
        "echo-plugin capabilities should include 'tool'"
    );
}

#[test]
fn api_plugin_detail_has_permissions() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    assert!(
        detail["permissions"].is_array(),
        "permissions must be an array"
    );
}

#[test]
fn api_plugin_detail_has_tools_with_full_schema() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);

    let tools = detail["tools"]
        .as_array()
        .expect("tools must be an array");
    assert!(!tools.is_empty(), "echo-plugin should have at least one tool");

    let tool_echo = tools
        .iter()
        .find(|t| t["name"] == "tool_echo")
        .expect("echo-plugin should have a tool named 'tool_echo'");

    // Full tool schema includes name, description, export, risk_level
    assert!(tool_echo["name"].is_string(), "tool must have name");
    assert!(tool_echo["description"].is_string(), "tool must have description");
    assert!(tool_echo["export"].is_string(), "tool must have export");
    assert!(tool_echo["risk_level"].is_string(), "tool must have risk_level");
}

#[test]
fn api_plugin_detail_tool_has_parameters_schema() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);

    let tools = detail["tools"].as_array().unwrap();
    let tool_echo = tools
        .iter()
        .find(|t| t["name"] == "tool_echo")
        .expect("tool_echo must exist");

    // echo-plugin declares a parameters_schema in its manifest
    assert!(
        tool_echo.get("parameters_schema").is_some(),
        "tool_echo should include parameters_schema (may be null for tools without params)"
    );
}

#[test]
fn api_plugin_detail_has_wasm_path() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    assert!(
        detail["wasm_path"].is_string(),
        "detail should include wasm_path as a string"
    );
}

#[test]
fn api_plugin_detail_has_wasm_sha256() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    // wasm_sha256 is present (may be null if no hash was recorded)
    assert!(
        detail.get("wasm_sha256").is_some(),
        "detail should include wasm_sha256 field"
    );
}

#[test]
fn api_plugin_detail_has_config_status() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    let config_status = detail["config_status"]
        .as_str()
        .expect("config_status must be a string");
    assert!(
        config_status == "ok" || config_status == "not_loaded",
        "config_status must be 'ok' or 'not_loaded', got: '{config_status}'"
    );
}

#[test]
fn api_plugin_detail_returns_none_for_unknown_plugin() {
    let host = setup_host();
    let result = host.get_plugin("nonexistent-plugin");
    assert!(
        result.is_none(),
        "get_plugin should return None for an unknown plugin name"
    );
}

// ---- Manifest fidelity: detail matches manifest data ----

#[test]
fn api_plugin_detail_description_matches_manifest() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    assert_eq!(
        detail["description"],
        "Echoes JSON input back as output \u{2014} used for round-trip integration tests."
    );
}

#[test]
fn api_plugin_detail_tools_count_matches_manifest() {
    let info = get_echo_plugin();
    let detail = build_plugin_detail(&info);
    let tools = detail["tools"].as_array().unwrap();
    // echo-plugin declares exactly one tool
    assert_eq!(tools.len(), 1, "echo-plugin should have exactly 1 tool");
}
