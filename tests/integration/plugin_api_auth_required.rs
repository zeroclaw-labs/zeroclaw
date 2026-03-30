//! Integration test: All plugin API endpoints require authentication.
//!
//! Verifies the acceptance criterion for US-ZCL-18:
//! > All endpoints require authentication
//!
//! Tests that `PairingGuard` (the auth mechanism used by the plugin endpoints)
//! correctly rejects unauthenticated and invalid requests when pairing is
//! required, and that both handler functions in `api_plugins.rs` invoke
//! `check_auth` before processing.

use zeroclaw::security::pairing::PairingGuard;

/// A known token that we seed into the guard for testing.
const TEST_TOKEN: &str = "zc_test_token_abc123";

/// Create a `PairingGuard` with pairing required and one pre-seeded token.
fn guard_with_pairing() -> PairingGuard {
    PairingGuard::new(true, &[TEST_TOKEN.to_string()])
}

// ── PairingGuard rejects unauthenticated requests ──────────────────────

#[test]
fn auth_rejects_empty_token_when_pairing_required() {
    let guard = guard_with_pairing();
    assert!(
        !guard.is_authenticated(""),
        "empty token must be rejected when pairing is required"
    );
}

#[test]
fn auth_rejects_garbage_token_when_pairing_required() {
    let guard = guard_with_pairing();
    assert!(
        !guard.is_authenticated("not-a-real-token"),
        "garbage token must be rejected when pairing is required"
    );
}

#[test]
fn auth_accepts_valid_token_when_pairing_required() {
    let guard = guard_with_pairing();
    assert!(
        guard.is_authenticated(TEST_TOKEN),
        "valid bearer token must be accepted"
    );
}

#[test]
fn auth_not_required_when_pairing_disabled() {
    let guard = PairingGuard::new(false, &[]);
    assert!(
        guard.is_authenticated(""),
        "any token should pass when pairing is disabled"
    );
}

// ── Source-level verification: both endpoints invoke check_auth ─────────

#[test]
fn list_plugins_handler_calls_check_auth() {
    let source = include_str!("../../src/gateway/api_plugins.rs");

    // Find the list_plugins function and verify it calls check_auth before
    // doing any work.
    let fn_start = source
        .find("pub async fn list_plugins")
        .expect("list_plugins handler should exist in api_plugins.rs");
    let fn_body = &source[fn_start..];

    let auth_offset = fn_body
        .find("check_auth")
        .expect("list_plugins must call check_auth");
    let config_offset = fn_body
        .find("state.config")
        .expect("list_plugins should access config");

    assert!(
        auth_offset < config_offset,
        "check_auth must be invoked before accessing state.config in list_plugins"
    );
}

#[test]
fn get_plugin_detail_handler_calls_check_auth() {
    let source = include_str!("../../src/gateway/api_plugins.rs");

    let fn_start = source
        .find("pub async fn get_plugin_detail")
        .expect("get_plugin_detail handler should exist in api_plugins.rs");
    let fn_body = &source[fn_start..];

    let auth_offset = fn_body
        .find("check_auth")
        .expect("get_plugin_detail must call check_auth");
    let config_offset = fn_body
        .find("state.config")
        .expect("get_plugin_detail should access config");

    assert!(
        auth_offset < config_offset,
        "check_auth must be invoked before accessing state.config in get_plugin_detail"
    );
}
