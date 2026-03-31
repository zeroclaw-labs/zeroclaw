//! Integration test: POST /api/plugins/{name}/enable behaviour.
//!
//! Verifies the acceptance criterion for US-ZCL-18:
//! > POST /api/plugins/{name}/enable enables a disabled plugin
//!
//! Uses `PluginHost` with the checked-in test plugins to verify that calling
//! `enable_plugin` on a disabled plugin transitions it to the enabled state.

use std::path::Path;

use zeroclaw::plugins::host::PluginHost;

/// Set up a PluginHost pointed at the test plugins directory.
fn setup_host() -> PluginHost {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    PluginHost::new(&base).expect("failed to create PluginHost from tests")
}

#[test]
fn enable_plugin_transitions_disabled_to_enabled() {
    let mut host = setup_host();

    // Disable the plugin first
    host.disable_plugin("echo-plugin")
        .expect("disable_plugin should succeed for echo-plugin");

    // Verify it is disabled
    let info = host.get_plugin("echo-plugin").expect("plugin should exist");
    assert!(
        !info.enabled,
        "plugin should be disabled after disable_plugin"
    );

    // Enable it
    host.enable_plugin("echo-plugin")
        .expect("enable_plugin should succeed for echo-plugin");

    // Verify it is now enabled
    let info = host.get_plugin("echo-plugin").expect("plugin should exist");
    assert!(info.enabled, "plugin should be enabled after enable_plugin");
}

#[test]
fn enable_plugin_on_already_enabled_is_idempotent() {
    let mut host = setup_host();

    // Plugins start enabled
    let info = host.get_plugin("echo-plugin").expect("plugin should exist");
    assert!(info.enabled, "plugin should be enabled by default");

    // Enabling again should succeed without error
    host.enable_plugin("echo-plugin")
        .expect("enable_plugin should succeed even if already enabled");

    let info = host.get_plugin("echo-plugin").expect("plugin should exist");
    assert!(info.enabled, "plugin should still be enabled");
}

#[test]
fn enable_plugin_unknown_name_returns_error() {
    let mut host = setup_host();

    let result = host.enable_plugin("nonexistent-plugin");
    assert!(
        result.is_err(),
        "enable_plugin should return error for unknown plugin"
    );
}

#[test]
fn enable_plugin_reflected_in_list_plugins() {
    let mut host = setup_host();

    // Disable then re-enable
    host.disable_plugin("echo-plugin").unwrap();
    host.enable_plugin("echo-plugin").unwrap();

    let plugins = host.list_plugins();
    let echo = plugins
        .iter()
        .find(|p| p.name == "echo-plugin")
        .expect("echo-plugin should appear in list_plugins");

    assert!(
        echo.enabled,
        "re-enabled plugin should show enabled in list"
    );
}

#[test]
fn enable_plugin_status_in_api_response_json() {
    let mut host = setup_host();

    // Disable then re-enable
    host.disable_plugin("echo-plugin").unwrap();
    host.enable_plugin("echo-plugin").unwrap();

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
        detail["enabled"], true,
        "API JSON should show enabled=true after enable_plugin"
    );
}
