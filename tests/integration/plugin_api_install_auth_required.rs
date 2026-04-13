#![cfg(feature = "plugins-wasm")]

//! Integration test: POST /api/plugins/install requires authentication.
//!
//! Verifies the acceptance criterion for US-ZCL-51:
//! > Endpoint requires authentication
//!
//! Tests that the `install_plugin` endpoint handler invokes `check_auth`
//! before performing any work, and that the `PairingGuard` correctly
//! rejects unauthenticated requests.

use zeroclaw::security::pairing::PairingGuard;

/// A known token that we seed into the guard for testing.
const TEST_TOKEN: &str = "zc_test_install_auth_token";

// ── PairingGuard behavior for install endpoint ───────────────────────────

#[test]
fn install_auth_rejects_empty_token() {
    let guard = PairingGuard::new(true, &[TEST_TOKEN.to_string()]);
    assert!(
        !guard.is_authenticated(""),
        "install endpoint must reject empty token when pairing is required"
    );
}

#[test]
fn install_auth_rejects_invalid_token() {
    let guard = PairingGuard::new(true, &[TEST_TOKEN.to_string()]);
    assert!(
        !guard.is_authenticated("invalid-token-xyz"),
        "install endpoint must reject invalid token when pairing is required"
    );
}

#[test]
fn install_auth_accepts_valid_token() {
    let guard = PairingGuard::new(true, &[TEST_TOKEN.to_string()]);
    assert!(
        guard.is_authenticated(TEST_TOKEN),
        "install endpoint must accept valid bearer token"
    );
}

// ── Source-level verification: install_plugin calls check_auth first ─────

#[test]
fn install_plugin_calls_check_auth_before_any_work() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("pub async fn install_plugin")
        .expect("install_plugin handler should exist in api_plugins.rs");
    let fn_body = &source[fn_start..];

    // check_auth must be called
    let auth_offset = fn_body
        .find("check_auth")
        .expect("install_plugin must call check_auth for authentication");

    // check_auth must be called before accessing config
    let config_offset = fn_body
        .find("state.config")
        .expect("install_plugin should access config");

    assert!(
        auth_offset < config_offset,
        "check_auth must be invoked before accessing state.config in install_plugin"
    );
}

#[test]
fn install_plugin_returns_early_on_auth_failure() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("pub async fn install_plugin")
        .expect("install_plugin handler should exist");
    let fn_body = &source[fn_start..];

    // Verify the pattern: if let Err(e) = check_auth(...) { return e.into_response(); }
    assert!(
        fn_body.contains("if let Err(e) = check_auth"),
        "install_plugin must handle auth failure with early return"
    );
    assert!(
        fn_body.contains("return e.into_response()"),
        "install_plugin must return the auth error response"
    );
}

// ── check_auth function returns UNAUTHORIZED status ──────────────────────

#[test]
fn check_auth_returns_unauthorized_on_failure() {
    let source = include_str!("../../crates/zeroclaw-gateway/src/api_plugins.rs");

    let fn_start = source
        .find("fn check_auth")
        .expect("check_auth function should exist");
    let fn_end = source[fn_start..]
        .find("\n    }")
        .map(|i| fn_start + i)
        .unwrap_or(source.len());
    let fn_body = &source[fn_start..fn_end];

    assert!(
        fn_body.contains("StatusCode::UNAUTHORIZED"),
        "check_auth must return UNAUTHORIZED status on authentication failure"
    );
    assert!(
        fn_body.contains("\"Unauthorized\""),
        "check_auth must return 'Unauthorized' message"
    );
}
