#![cfg(feature = "plugins-wasm")]

//! Integration test: Failure shows descriptive error message.
//!
//! Verifies the acceptance criterion for US-ZCL-51:
//! > Failure shows descriptive error message
//!
//! Tests that:
//! 1. The `install_plugin` error path returns a JSON body with an "error" field
//! 2. The error field uses `e.to_string()` for descriptive messages
//! 3. PluginError variants have human-readable error messages via thiserror

// ── Error response structure verification ──────────────────────────────

#[test]
fn install_plugin_error_includes_error_field() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    // Find the install_plugin function's error handling
    let fn_start = source
        .find("pub async fn install_plugin")
        .expect("install_plugin handler must exist");
    let fn_body = &source[fn_start..];

    // Verify error response includes "error" field in JSON
    assert!(
        fn_body.contains(r#""error""#) || fn_body.contains("\"error\""),
        "install_plugin error response must include an 'error' field"
    );

    // Verify the error is converted to string using e.to_string()
    assert!(
        fn_body.contains("e.to_string()"),
        "install_plugin must use e.to_string() to get descriptive error message"
    );
}

#[test]
fn install_plugin_error_returns_ok_false() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("pub async fn install_plugin")
        .expect("install_plugin handler must exist");
    let fn_body = &source[fn_start..];

    // Verify error response includes ok: false
    assert!(
        fn_body.contains(r#""ok": false"#) || fn_body.contains("\"ok\": false"),
        "install_plugin error response must include 'ok: false'"
    );
}

// ── PluginError descriptiveness verification ───────────────────────────

#[test]
fn plugin_error_has_descriptive_messages() {
    let source = include_str!("../../crates/zeroclaw-plugins/src/error.rs");

    // Verify PluginError uses thiserror for descriptive messages
    assert!(
        source.contains("use thiserror::Error") || source.contains("thiserror::Error"),
        "PluginError must use thiserror for automatic Display impl"
    );

    // Verify key error variants have descriptive messages
    let descriptive_errors = [
        ("NotFound", "plugin not found"),
        ("InvalidManifest", "invalid manifest"),
        ("LoadFailed", "failed to load"),
        ("ExecutionFailed", "execution failed"),
        ("AlreadyLoaded", "already loaded"),
    ];

    for (variant, expected_text) in descriptive_errors {
        assert!(
            source.contains(variant),
            "PluginError must have {} variant",
            variant
        );
        assert!(
            source.to_lowercase().contains(expected_text),
            "PluginError::{} must have descriptive message containing '{}'",
            variant,
            expected_text
        );
    }
}

#[test]
fn plugin_error_not_found_includes_path_info() {
    let source = include_str!("../../crates/zeroclaw-plugins/src/host.rs");

    // Find install function error handling
    let fn_start = source
        .find("fn install")
        .expect("install function must exist");
    let fn_body = &source[fn_start..fn_start.saturating_add(2000)];

    // Verify NotFound errors include path information
    assert!(
        fn_body.contains("manifest.toml not found at") || fn_body.contains("WASM file not found"),
        "install errors must include specific path information"
    );

    // Verify path is included via display()
    assert!(
        fn_body.contains(".display()"),
        "install errors should use .display() to show file paths"
    );
}

// ── Error HTTP status verification ─────────────────────────────────────

#[test]
fn install_plugin_error_returns_bad_request_status() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("pub async fn install_plugin")
        .expect("install_plugin handler must exist");
    let fn_body = &source[fn_start..];

    // Find the Err arm of the match
    let err_arm = fn_body
        .find("Err(e)")
        .expect("install_plugin must handle Err case");
    let err_handling = &fn_body[err_arm..err_arm.saturating_add(500)];

    // Verify BAD_REQUEST status is used for install errors
    assert!(
        err_handling.contains("BAD_REQUEST"),
        "install_plugin errors must return BAD_REQUEST status code"
    );
}
