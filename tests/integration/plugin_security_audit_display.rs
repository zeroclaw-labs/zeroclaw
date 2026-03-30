//! Integration test: Security audit summary displays network and filesystem capabilities.
//!
//! Verifies the acceptance criterion for US-ZCL-21:
//! > Security audit summary displays network and filesystem capabilities
//!
//! Uses `PluginHost` with the checked-in test plugins to verify that the API
//! response JSON includes `allowed_hosts`, `allowed_paths`, and `tools` with
//! `risk_level` — the three data sources the PluginDetail audit section renders.

use std::path::Path;

use zeroclaw::plugins::host::PluginHost;
use zeroclaw::plugins::PluginInfo;

/// Set up a PluginHost pointed at the test plugins directory.
fn setup_host() -> PluginHost {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    PluginHost::new(&base).expect("failed to create PluginHost from tests")
}

/// Build the audit-relevant JSON in the same shape as the GET /api/plugins/{name} endpoint.
fn build_audit_json(info: &PluginInfo) -> serde_json::Value {
    serde_json::json!({
        "name": info.name,
        "allowed_hosts": info.allowed_hosts,
        "allowed_paths": info.allowed_paths,
        "tools": info.tools,
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

fn get_fs_plugin() -> PluginInfo {
    let host = setup_host();
    host.get_plugin("fs-plugin")
        .expect("fs-plugin should be discovered from test plugins")
}

// ---- Network capabilities (allowed_hosts) ----

#[test]
fn audit_display_multi_tool_has_allowed_hosts_array() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    assert!(
        detail["allowed_hosts"].is_array(),
        "allowed_hosts must be a JSON array"
    );
}

#[test]
fn audit_display_multi_tool_has_expected_hosts() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    let hosts: Vec<&str> = detail["allowed_hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(hosts.contains(&"httpbin.org"), "should contain httpbin.org");
    assert!(hosts.contains(&"example.com"), "should contain example.com");
}

#[test]
fn audit_display_multi_tool_host_count() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    let hosts = detail["allowed_hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 2, "multi-tool-plugin declares exactly 2 allowed hosts");
}

#[test]
fn audit_display_echo_plugin_has_no_hosts() {
    let info = get_echo_plugin();
    let detail = build_audit_json(&info);
    let hosts = detail["allowed_hosts"]
        .as_array()
        .expect("allowed_hosts must be a JSON array");
    assert!(
        hosts.is_empty(),
        "echo-plugin should have no allowed hosts (UI shows 'No network access')"
    );
}

// ---- Filesystem capabilities (allowed_paths) ----

#[test]
fn audit_display_multi_tool_has_allowed_paths_object() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    assert!(
        detail["allowed_paths"].is_object(),
        "allowed_paths must be a JSON object"
    );
}

#[test]
fn audit_display_multi_tool_has_expected_paths() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    let paths = detail["allowed_paths"]
        .as_object()
        .unwrap();
    assert!(
        paths.contains_key("/data"),
        "should contain /data path mapping"
    );
    assert!(
        paths.contains_key("/config"),
        "should contain /config path mapping"
    );
}

#[test]
fn audit_display_multi_tool_path_values_are_host_paths() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    let paths = detail["allowed_paths"].as_object().unwrap();
    assert_eq!(
        paths["/data"].as_str().unwrap(),
        "/tmp/zeroclaw-test-data",
        "/data should map to /tmp/zeroclaw-test-data"
    );
    assert_eq!(
        paths["/config"].as_str().unwrap(),
        "/tmp/zeroclaw-test-config",
        "/config should map to /tmp/zeroclaw-test-config"
    );
}

#[test]
fn audit_display_fs_plugin_has_filesystem_paths() {
    let info = get_fs_plugin();
    let detail = build_audit_json(&info);
    let paths = detail["allowed_paths"]
        .as_object()
        .expect("allowed_paths must be a JSON object");
    assert_eq!(paths.len(), 2, "fs-plugin declares exactly 2 filesystem paths");
    assert!(paths.contains_key("/input"), "should contain /input");
    assert!(paths.contains_key("/output"), "should contain /output");
}

#[test]
fn audit_display_echo_plugin_has_no_paths() {
    let info = get_echo_plugin();
    let detail = build_audit_json(&info);
    let paths = detail["allowed_paths"]
        .as_object()
        .expect("allowed_paths must be a JSON object");
    assert!(
        paths.is_empty(),
        "echo-plugin should have no allowed paths (UI shows 'No filesystem access')"
    );
}

// ---- Risk level breakdown (tools with risk_level) ----

#[test]
fn audit_display_multi_tool_tools_have_risk_level() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    let tools = detail["tools"].as_array().unwrap();
    assert!(!tools.is_empty(), "multi-tool-plugin should have tools");
    for tool in tools {
        let risk = tool["risk_level"].as_str().unwrap_or("");
        assert!(
            risk == "low" || risk == "medium" || risk == "high",
            "tool {} has invalid risk_level: '{risk}'",
            tool["name"]
        );
    }
}

#[test]
fn audit_display_multi_tool_has_low_risk_tools() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    let tools = detail["tools"].as_array().unwrap();
    let low_count = tools.iter().filter(|t| t["risk_level"] == "low").count();
    assert!(
        low_count > 0,
        "multi-tool-plugin should have at least one low-risk tool"
    );
}

#[test]
fn audit_display_multi_tool_has_medium_risk_tools() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    let tools = detail["tools"].as_array().unwrap();
    let medium_count = tools.iter().filter(|t| t["risk_level"] == "medium").count();
    assert!(
        medium_count > 0,
        "multi-tool-plugin should have at least one medium-risk tool"
    );
}

#[test]
fn audit_display_echo_plugin_tools_have_risk_level() {
    let info = get_echo_plugin();
    let detail = build_audit_json(&info);
    let tools = detail["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "echo-plugin has exactly one tool");
    assert_eq!(
        tools[0]["risk_level"], "low",
        "echo-plugin's tool_echo should be low risk"
    );
}

// ---- Combined audit: plugin with both network + filesystem ----

#[test]
fn audit_display_multi_tool_has_both_network_and_filesystem() {
    let info = get_multi_tool_plugin();
    let detail = build_audit_json(&info);
    let has_hosts = !detail["allowed_hosts"].as_array().unwrap().is_empty();
    let has_paths = !detail["allowed_paths"].as_object().unwrap().is_empty();
    assert!(
        has_hosts && has_paths,
        "multi-tool-plugin should have both network and filesystem capabilities for a full audit display"
    );
}

#[test]
fn audit_display_echo_plugin_has_neither_network_nor_filesystem() {
    let info = get_echo_plugin();
    let detail = build_audit_json(&info);
    let has_hosts = !detail["allowed_hosts"].as_array().unwrap().is_empty();
    let has_paths = !detail["allowed_paths"].as_object().unwrap().is_empty();
    assert!(
        !has_hosts && !has_paths,
        "echo-plugin should have neither network nor filesystem capabilities (UI shows safe badges)"
    );
}

// ---- Filesystem-only plugin ----

#[test]
fn audit_display_fs_plugin_has_filesystem_but_no_network() {
    let info = get_fs_plugin();
    let detail = build_audit_json(&info);
    let has_hosts = !detail["allowed_hosts"].as_array().unwrap().is_empty();
    let has_paths = !detail["allowed_paths"].as_object().unwrap().is_empty();
    assert!(!has_hosts, "fs-plugin should have no network access");
    assert!(has_paths, "fs-plugin should have filesystem access");
}
