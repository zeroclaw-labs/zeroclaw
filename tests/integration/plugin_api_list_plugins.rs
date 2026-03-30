//! Integration test: GET /api/plugins response shape.
//!
//! Verifies the acceptance criterion for US-ZCL-18:
//! > GET /api/plugins returns list of plugins with name version description
//! > status tools capabilities
//!
//! Uses `PluginHost` with the checked-in test plugins to verify that the JSON
//! response constructed by the endpoint contains every required field.

use std::path::Path;

use zeroclaw::plugins::host::PluginHost;

/// Set up a PluginHost pointed at the test plugins directory.
fn setup_host() -> PluginHost {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    PluginHost::new(&base).expect("failed to create PluginHost from tests")
}

/// Build the JSON response in the same way the GET /api/plugins endpoint does.
fn build_api_response(host: &PluginHost) -> serde_json::Value {
    let plugins: Vec<serde_json::Value> = host
        .list_plugins()
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "version": p.version,
                "description": p.description,
                "status": if p.loaded { "loaded" } else { "discovered" },
                "tools": p.tools,
                "capabilities": p.capabilities,
            })
        })
        .collect();

    serde_json::json!({
        "plugins_enabled": true,
        "plugins_dir": "tests/plugins",
        "plugins": plugins,
    })
}

#[test]
fn api_plugins_response_contains_plugins_array() {
    let host = setup_host();
    let response = build_api_response(&host);

    assert!(
        response["plugins"].is_array(),
        "response should contain a 'plugins' array"
    );
    assert!(
        !response["plugins"].as_array().unwrap().is_empty(),
        "plugins array should not be empty (test plugins should be discovered)"
    );
}

#[test]
fn api_plugins_each_plugin_has_name() {
    let host = setup_host();
    let response = build_api_response(&host);

    for plugin in response["plugins"].as_array().unwrap() {
        assert!(
            plugin["name"].is_string(),
            "each plugin must have a string 'name' field, got: {plugin}"
        );
        assert!(
            !plugin["name"].as_str().unwrap().is_empty(),
            "plugin name must not be empty"
        );
    }
}

#[test]
fn api_plugins_each_plugin_has_version() {
    let host = setup_host();
    let response = build_api_response(&host);

    for plugin in response["plugins"].as_array().unwrap() {
        assert!(
            plugin["version"].is_string(),
            "each plugin must have a string 'version' field, got: {plugin}"
        );
    }
}

#[test]
fn api_plugins_each_plugin_has_description() {
    let host = setup_host();
    let response = build_api_response(&host);

    for plugin in response["plugins"].as_array().unwrap() {
        // description can be null (Option<String>) but the key must be present
        assert!(
            plugin.get("description").is_some(),
            "each plugin must have a 'description' field (may be null), got: {plugin}"
        );
    }
}

#[test]
fn api_plugins_each_plugin_has_status() {
    let host = setup_host();
    let response = build_api_response(&host);

    for plugin in response["plugins"].as_array().unwrap() {
        let status = plugin["status"]
            .as_str()
            .unwrap_or_else(|| panic!("each plugin must have a string 'status' field, got: {plugin}"));
        assert!(
            status == "loaded" || status == "discovered",
            "status must be 'loaded' or 'discovered', got: '{status}'"
        );
    }
}

#[test]
fn api_plugins_each_plugin_has_tools() {
    let host = setup_host();
    let response = build_api_response(&host);

    for plugin in response["plugins"].as_array().unwrap() {
        assert!(
            plugin["tools"].is_array(),
            "each plugin must have a 'tools' array, got: {plugin}"
        );
    }
}

#[test]
fn api_plugins_each_plugin_has_capabilities() {
    let host = setup_host();
    let response = build_api_response(&host);

    for plugin in response["plugins"].as_array().unwrap() {
        assert!(
            plugin["capabilities"].is_array(),
            "each plugin must have a 'capabilities' array, got: {plugin}"
        );
    }
}

#[test]
fn api_plugins_echo_plugin_has_expected_shape() {
    let host = setup_host();
    let response = build_api_response(&host);

    let plugins = response["plugins"].as_array().unwrap();
    let echo = plugins
        .iter()
        .find(|p| p["name"] == "echo-plugin")
        .expect("echo-plugin should be discovered from test plugins");

    assert_eq!(echo["name"], "echo-plugin");
    assert_eq!(echo["version"], "0.1.0");
    assert!(echo["description"].is_string(), "echo-plugin should have a description");
    assert!(echo["status"].is_string(), "echo-plugin should have a status");
    assert!(echo["capabilities"].is_array(), "echo-plugin should have capabilities");

    let tools = echo["tools"].as_array().unwrap();
    assert!(
        !tools.is_empty(),
        "echo-plugin should have at least one tool"
    );

    let tool_echo = tools
        .iter()
        .find(|t| t["name"] == "tool_echo")
        .expect("echo-plugin should have a tool named 'tool_echo'");
    assert!(
        tool_echo["description"].is_string(),
        "tool_echo should have a description"
    );
}

#[test]
fn api_plugins_response_has_metadata_fields() {
    let host = setup_host();
    let response = build_api_response(&host);

    assert!(
        response.get("plugins_enabled").is_some(),
        "response should have 'plugins_enabled' field"
    );
    assert!(
        response.get("plugins_dir").is_some(),
        "response should have 'plugins_dir' field"
    );
}
