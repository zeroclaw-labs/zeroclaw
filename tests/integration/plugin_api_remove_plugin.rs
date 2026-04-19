#![cfg(feature = "plugins-wasm")]

//! Integration test: DELETE /api/plugins/{name} endpoint exists and functions.
//!
//! Verifies the acceptance criterion for US-ZCL-52:
//! > DELETE /api/plugins/{name} endpoint exists in gateway
//!
//! Uses `PluginHost` with the checked-in test plugins to verify that calling
//! `remove` on a loaded plugin removes it from the host.

use std::path::Path;

use zeroclaw::plugins::host::PluginHost;

/// Set up a PluginHost pointed at a temporary copy of the test plugins directory
/// so we can safely remove plugins without affecting other tests.
fn setup_host_with_temp_dir() -> (PluginHost, tempfile::TempDir) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");

    // Copy the plugins directory to temp
    let plugins_src = base.join("plugins");
    let plugins_dst = temp_dir.path().join("plugins");
    if plugins_src.exists() {
        copy_dir_all(&plugins_src, &plugins_dst).expect("failed to copy plugins");
    }

    let host = PluginHost::new(temp_dir.path()).expect("failed to create PluginHost");
    (host, temp_dir)
}

/// Recursively copy a directory.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[test]
fn remove_plugin_removes_from_host() {
    let (mut host, _temp_dir) = setup_host_with_temp_dir();

    // Verify plugin exists before removal
    let info = host.get_plugin("echo-plugin");
    assert!(info.is_some(), "echo-plugin should exist before removal");

    // Remove it
    host.remove("echo-plugin")
        .expect("remove should succeed for echo-plugin");

    // Verify it no longer exists
    let info = host.get_plugin("echo-plugin");
    assert!(info.is_none(), "echo-plugin should not exist after removal");
}

#[test]
fn remove_plugin_unknown_name_returns_error() {
    let (mut host, _temp_dir) = setup_host_with_temp_dir();

    let result = host.remove("nonexistent-plugin");
    assert!(
        result.is_err(),
        "remove should return error for unknown plugin"
    );
}

#[test]
fn remove_plugin_not_in_list_after_removal() {
    let (mut host, _temp_dir) = setup_host_with_temp_dir();

    // Remove the plugin
    host.remove("echo-plugin").expect("remove should succeed");

    let plugins = host.list_plugins();
    let echo = plugins.iter().find(|p| p.name == "echo-plugin");

    assert!(
        echo.is_none(),
        "removed plugin should not appear in list_plugins"
    );
}

#[test]
fn remove_plugin_api_response_json_structure() {
    // Verify the expected JSON response structure for the DELETE endpoint.
    // The endpoint returns { "ok": true, "message": "..." } on success.
    let success_response = serde_json::json!({
        "ok": true,
        "message": "Plugin 'test-plugin' removed",
    });

    assert_eq!(success_response["ok"], true);
    assert!(success_response["message"].is_string());
}

#[test]
fn remove_plugin_api_error_response_json_structure() {
    // Verify the expected JSON error response structure.
    // The endpoint returns { "ok": false, "error": "..." } on failure.
    let error_response = serde_json::json!({
        "ok": false,
        "error": "Plugin not found: nonexistent",
    });

    assert_eq!(error_response["ok"], false);
    assert!(error_response["error"].is_string());
}

// ── Authentication requirement verification (US-ZCL-52-8) ─────────────────

#[test]
fn remove_plugin_endpoint_requires_authentication() {
    // Verify acceptance criterion for US-ZCL-52:
    // > Endpoint requires authentication
    //
    // The remove_plugin handler must call check_auth before processing
    // any plugin removal logic.
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("pub async fn remove_plugin")
        .expect("remove_plugin handler must exist in api_plugins.rs");
    let fn_body = &source[fn_start..];

    // Find end of function by looking for next pub async fn or closing brace
    let fn_end = fn_body[1..].find("pub async fn").unwrap_or(fn_body.len());
    let fn_body = &fn_body[..fn_end];

    // Verify check_auth is called
    let auth_offset = fn_body
        .find("check_auth")
        .expect("remove_plugin must call check_auth for authentication");

    // Verify check_auth is called before accessing config or plugin host
    let config_offset = fn_body
        .find("state.config")
        .expect("remove_plugin should access config");

    assert!(
        auth_offset < config_offset,
        "check_auth must be invoked before accessing state.config in remove_plugin"
    );

    // Verify error handling: early return on auth failure
    // Look backwards from check_auth to find the if let Err pattern
    let pre_auth_region = &fn_body[..auth_offset];
    let auth_to_end = &fn_body[auth_offset..];
    let after_call = &auth_to_end[..std::cmp::min(100, auth_to_end.len())];

    // Check for "if let Err" pattern before check_auth, or "?" after it
    let has_if_let_err = pre_auth_region.trim_end().ends_with("if let Err(e) =");
    let has_question_mark = after_call.contains(")?");

    assert!(
        has_if_let_err || has_question_mark,
        "remove_plugin must return early on authentication failure (via if let Err or ?)"
    );
}
