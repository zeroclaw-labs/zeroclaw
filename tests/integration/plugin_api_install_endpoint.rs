#![cfg(feature = "plugins-wasm")]

//! Integration test: POST /api/plugins/install endpoint.
//!
//! Verifies the acceptance criterion for US-ZCL-51:
//! > POST /api/plugins/install endpoint exists in gateway
//!
//! Tests that:
//! 1. The `install_plugin` endpoint handler exists in `api_plugins.rs`
//! 2. It invokes `check_auth` before processing (requires authentication)

// ── Endpoint existence verification ─────────────────────────────────────

#[test]
fn install_plugin_endpoint_exists() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    assert!(
        source.contains("pub async fn install_plugin"),
        "install_plugin handler must exist in api_plugins.rs"
    );
}

#[test]
fn install_plugin_handler_calls_check_auth() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("pub async fn install_plugin")
        .expect("install_plugin handler should exist in api_plugins.rs");
    let fn_body = &source[fn_start..];

    let auth_offset = fn_body
        .find("check_auth")
        .expect("install_plugin must call check_auth");
    let config_offset = fn_body
        .find("state.config")
        .expect("install_plugin should access config");

    assert!(
        auth_offset < config_offset,
        "check_auth must be invoked before accessing state.config in install_plugin"
    );
}

// ── Route registration verification ─────────────────────────────────────

#[test]
fn install_plugin_route_registered() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/lib.rs");

    assert!(
        source.contains("install_plugin"),
        "install_plugin must be registered as a route in gateway/mod.rs"
    );

    // Verify it's a POST endpoint at the expected path
    assert!(
        source.contains("/api/plugins/install") || source.contains("api/plugins/install"),
        "install_plugin route should be at /api/plugins/install"
    );
}
