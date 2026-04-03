#![cfg(feature = "plugins-wasm")]

//! Security test: mounting /etc/passwd is rejected at load time.
//!
//! Verifies that `validate_allowed_paths` rejects a plugin manifest that
//! attempts to mount `/etc/passwd` (or any path under `/etc`) into the WASI
//! guest, using the default `FORBIDDEN_PATHS` list. This ensures the security
//! check fires before the plugin is ever instantiated.

use std::collections::HashMap;

use zeroclaw::plugins::loader::{FORBIDDEN_PATHS, validate_allowed_paths};

/// Build the forbidden list the same way the real loader does.
fn forbidden_list() -> Vec<String> {
    FORBIDDEN_PATHS.iter().map(|s| (*s).to_string()).collect()
}

/// Mounting `/etc/passwd` as a guest path should be rejected at load time.
#[test]
fn mount_etc_passwd_rejected_at_load_time() {
    let mut allowed = HashMap::new();
    allowed.insert("/secrets".to_string(), "/etc/passwd".to_string());

    let forbidden = forbidden_list();
    let result = validate_allowed_paths("evil-plugin", &allowed, &forbidden);

    assert!(
        result.is_err(),
        "mounting /etc/passwd should be rejected at load time, but was allowed"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("/etc/passwd"),
        "error message should mention the forbidden path: {err_msg}"
    );
    assert!(
        err_msg.contains("evil-plugin"),
        "error message should name the plugin: {err_msg}"
    );
}

/// Mounting `/etc` itself should also be rejected.
#[test]
fn mount_etc_directory_rejected_at_load_time() {
    let mut allowed = HashMap::new();
    allowed.insert("/config".to_string(), "/etc".to_string());

    let forbidden = forbidden_list();
    let result = validate_allowed_paths("etc-mount-plugin", &allowed, &forbidden);

    assert!(
        result.is_err(),
        "mounting /etc should be rejected at load time"
    );
}

/// A safe path (e.g. a temp directory) should be allowed through.
#[test]
fn safe_mount_is_allowed() {
    let mut allowed = HashMap::new();
    allowed.insert("/data".to_string(), "/tmp/plugin-workspace".to_string());

    let forbidden = forbidden_list();
    let result = validate_allowed_paths("safe-plugin", &allowed, &forbidden);

    assert!(
        result.is_ok(),
        "mounting /tmp/plugin-workspace should be allowed, but got: {:?}",
        result.unwrap_err()
    );
}
