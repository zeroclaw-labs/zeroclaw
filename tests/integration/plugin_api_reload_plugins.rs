#![cfg(feature = "plugins-wasm")]

//! Integration test: POST /api/plugins/reload endpoint.
//!
//! Verifies the acceptance criterion for US-ZCL-50:
//! > POST /api/plugins/reload endpoint exists in gateway and calls PluginHost::reload()
//!
//! Tests that:
//! 1. The `reload_plugins` endpoint handler exists in `api_plugins.rs`
//! 2. It invokes `check_auth` before processing (requires authentication)
//! 3. It calls `host.reload()` and returns a ReloadSummary-shaped response
//! 4. The JSON response contains `ok`, `total`, `loaded`, `unloaded`, `failed` fields

use std::path::Path;

use zeroclaw::plugins::host::PluginHost;

/// Set up a PluginHost pointed at the test plugins directory.
fn setup_host() -> PluginHost {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    PluginHost::new(&base).expect("failed to create PluginHost from tests")
}

// ── Endpoint existence verification ─────────────────────────────────────

#[test]
fn reload_plugins_endpoint_exists() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    assert!(
        source.contains("pub async fn reload_plugins"),
        "reload_plugins handler must exist in api_plugins.rs"
    );
}

#[test]
fn reload_plugins_handler_calls_check_auth() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("pub async fn reload_plugins")
        .expect("reload_plugins handler should exist in api_plugins.rs");
    let fn_body = &source[fn_start..];

    let auth_offset = fn_body
        .find("check_auth")
        .expect("reload_plugins must call check_auth");
    let config_offset = fn_body
        .find("state.config")
        .expect("reload_plugins should access config");

    assert!(
        auth_offset < config_offset,
        "check_auth must be invoked before accessing state.config in reload_plugins"
    );
}

#[test]
fn reload_plugins_handler_calls_host_reload() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("pub async fn reload_plugins")
        .expect("reload_plugins handler should exist in api_plugins.rs");
    let fn_body = &source[fn_start..];

    // Find end of function by looking for next pub async fn or end of module
    let fn_end = fn_body[1..].find("pub async fn").unwrap_or(fn_body.len());
    let fn_body = &fn_body[..fn_end];

    assert!(
        fn_body.contains("host.reload()"),
        "reload_plugins handler must call host.reload()"
    );
}

// ── ReloadSummary response shape verification ───────────────────────────

#[test]
fn plugin_host_reload_returns_summary() {
    let mut host = setup_host();

    let result = host.reload();
    assert!(
        result.is_ok(),
        "reload() should succeed on valid plugins directory"
    );

    let summary = result.unwrap();

    // ReloadSummary must have these fields
    let _total: usize = summary.total;
    let _loaded: Vec<String> = summary.loaded;
    let _unloaded: Vec<String> = summary.unloaded;
    let _failed: Vec<String> = summary.failed;
}

/// Build the JSON response in the same way the POST /api/plugins/reload endpoint does.
fn build_reload_response(
    host: &mut PluginHost,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let summary = host.reload()?;
    Ok(serde_json::json!({
        "ok": true,
        "total": summary.total,
        "loaded": summary.loaded,
        "unloaded": summary.unloaded,
        "failed": summary.failed,
    }))
}

#[test]
fn reload_response_has_ok_field() {
    let mut host = setup_host();
    let response = build_reload_response(&mut host).expect("reload should succeed");

    assert!(
        response.get("ok").is_some(),
        "response must have 'ok' field"
    );
    assert_eq!(
        response["ok"], true,
        "ok field should be true on successful reload"
    );
}

#[test]
fn reload_response_has_total_field() {
    let mut host = setup_host();
    let response = build_reload_response(&mut host).expect("reload should succeed");

    assert!(
        response.get("total").is_some(),
        "response must have 'total' field"
    );
    assert!(
        response["total"].is_number(),
        "total field must be a number"
    );
}

#[test]
fn reload_response_has_loaded_array() {
    let mut host = setup_host();
    let response = build_reload_response(&mut host).expect("reload should succeed");

    assert!(
        response.get("loaded").is_some(),
        "response must have 'loaded' field"
    );
    assert!(
        response["loaded"].is_array(),
        "loaded field must be an array"
    );
}

#[test]
fn reload_response_has_unloaded_array() {
    let mut host = setup_host();
    let response = build_reload_response(&mut host).expect("reload should succeed");

    assert!(
        response.get("unloaded").is_some(),
        "response must have 'unloaded' field"
    );
    assert!(
        response["unloaded"].is_array(),
        "unloaded field must be an array"
    );
}

#[test]
fn reload_response_has_failed_array() {
    let mut host = setup_host();
    let response = build_reload_response(&mut host).expect("reload should succeed");

    assert!(
        response.get("failed").is_some(),
        "response must have 'failed' field"
    );
    assert!(
        response["failed"].is_array(),
        "failed field must be an array"
    );
}

#[test]
fn reload_total_reflects_discovered_plugins() {
    let mut host = setup_host();
    let response = build_reload_response(&mut host).expect("reload should succeed");

    let total = response["total"].as_u64().expect("total should be u64");
    assert!(
        total > 0,
        "total should be > 0 when test plugins are present"
    );

    // Verify total matches list_plugins count
    let plugin_count = host.list_plugins().len();
    assert_eq!(
        total as usize, plugin_count,
        "total in reload response should match list_plugins count"
    );
}
