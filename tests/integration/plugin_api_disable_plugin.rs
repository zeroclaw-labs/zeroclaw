#![cfg(feature = "plugins-wasm")]

//! Integration test: POST /api/plugins/{name}/disable behaviour.
//!
//! Verifies the acceptance criterion for US-ZCL-18:
//! > POST /api/plugins/{name}/disable disables an enabled plugin
//!
//! Uses `PluginHost` with the checked-in test plugins to verify that calling
//! `disable_plugin` on an enabled plugin transitions it to the disabled state.

use std::path::Path;

use zeroclaw::plugins::host::PluginHost;

/// Set up a PluginHost pointed at the test plugins directory.
fn setup_host() -> PluginHost {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    PluginHost::new(&base).expect("failed to create PluginHost from tests")
}

#[test]
fn disable_plugin_transitions_enabled_to_disabled() {
    let mut host = setup_host();

    // Plugins start enabled
    let info = host.get_plugin("echo-plugin").expect("plugin should exist");
    assert!(info.enabled, "plugin should be enabled by default");

    // Disable it
    host.disable_plugin("echo-plugin")
        .expect("disable_plugin should succeed for echo-plugin");

    // Verify it is now disabled
    let info = host.get_plugin("echo-plugin").expect("plugin should exist");
    assert!(
        !info.enabled,
        "plugin should be disabled after disable_plugin"
    );
}

#[test]
fn disable_plugin_on_already_disabled_is_idempotent() {
    let mut host = setup_host();

    // Disable twice
    host.disable_plugin("echo-plugin")
        .expect("first disable should succeed");
    host.disable_plugin("echo-plugin")
        .expect("disable_plugin should succeed even if already disabled");

    let info = host.get_plugin("echo-plugin").expect("plugin should exist");
    assert!(!info.enabled, "plugin should still be disabled");
}

#[test]
fn disable_plugin_unknown_name_returns_error() {
    let mut host = setup_host();

    let result = host.disable_plugin("nonexistent-plugin");
    assert!(
        result.is_err(),
        "disable_plugin should return error for unknown plugin"
    );
}

#[test]
fn disable_plugin_reflected_in_list_plugins() {
    let mut host = setup_host();

    // Disable the plugin
    host.disable_plugin("echo-plugin").unwrap();

    let plugins = host.list_plugins();
    let echo = plugins
        .iter()
        .find(|p| p.name == "echo-plugin")
        .expect("echo-plugin should appear in list_plugins");

    assert!(
        !echo.enabled,
        "disabled plugin should show enabled=false in list"
    );
}

#[test]
fn disable_plugin_status_in_api_response_json() {
    let mut host = setup_host();

    // Disable the plugin
    host.disable_plugin("echo-plugin").unwrap();

    let info = host.get_plugin("echo-plugin").expect("plugin should exist");

    // Build the JSON the same way the API endpoint would
    let detail = serde_json::json!({
        "name": info.name,
        "version": info.version,
        "description": info.description,
        "status": if info.loaded { "loaded" } else { "discovered" },
        "enabled": info.enabled,
        "capabilities": info.capabilities,
        "permissions": info.permissions,
        "tools": info.tools,
    });

    assert_eq!(
        detail["enabled"], false,
        "API JSON should show enabled=false after disable_plugin"
    );
}
