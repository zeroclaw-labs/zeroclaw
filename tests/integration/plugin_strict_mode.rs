#![cfg(feature = "plugins-wasm")]

//! Security test: strict mode rejects wildcard hosts and limits filesystem
//! access to the workspace subtree.
//!
//! Verifies the two key strict-mode behaviours from US-ZCL-15:
//! 1. `validate_allowed_hosts` rejects any host pattern containing `*` at the
//!    `Strict` security level.
//! 2. `validate_workspace_paths` rejects physical paths that fall outside the
//!    workspace root directory.

use std::collections::HashMap;
use std::path::Path;

use zeroclaw::plugins::loader::{
    NetworkSecurityLevel, validate_allowed_hosts, validate_workspace_paths,
};

// ── Wildcard host rejection ────────────────────────────────────────────

#[test]
fn strict_rejects_wildcard_star_host() {
    let hosts = vec!["*.example.com".to_string()];
    let result = validate_allowed_hosts("test-plugin", &hosts, NetworkSecurityLevel::Strict);

    assert!(
        result.is_err(),
        "strict mode should reject wildcard host *.example.com"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("test-plugin"),
        "error should name the plugin: {err_msg}"
    );
    assert!(
        err_msg.contains("*.example.com"),
        "error should name the rejected host: {err_msg}"
    );
    assert!(
        err_msg.contains("Strict"),
        "error should mention the security level: {err_msg}"
    );
}

#[test]
fn strict_rejects_bare_star_host() {
    let hosts = vec!["*".to_string()];
    let result = validate_allowed_hosts("greedy-plugin", &hosts, NetworkSecurityLevel::Strict);

    assert!(
        result.is_err(),
        "strict mode should reject bare wildcard host '*'"
    );
}

#[test]
fn strict_rejects_wildcard_among_valid_hosts() {
    let hosts = vec![
        "api.example.com".to_string(),
        "*.internal.io".to_string(),
        "cdn.example.com".to_string(),
    ];
    let result = validate_allowed_hosts("mixed-plugin", &hosts, NetworkSecurityLevel::Strict);

    assert!(
        result.is_err(),
        "strict mode should reject when any host is a wildcard, even if others are valid"
    );
}

#[test]
fn strict_allows_explicit_hosts() {
    let hosts = vec!["api.example.com".to_string(), "cdn.example.com".to_string()];
    let result = validate_allowed_hosts("good-plugin", &hosts, NetworkSecurityLevel::Strict);

    assert!(
        result.is_ok(),
        "strict mode should allow explicit (non-wildcard) hosts, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn strict_allows_empty_hosts_list() {
    let hosts: Vec<String> = vec![];
    let result = validate_allowed_hosts("no-net-plugin", &hosts, NetworkSecurityLevel::Strict);

    assert!(
        result.is_ok(),
        "strict mode should allow an empty hosts list"
    );
}

// ── Workspace subtree enforcement ──────────────────────────────────────

#[test]
fn strict_rejects_absolute_path_outside_workspace() {
    let mut allowed = HashMap::new();
    allowed.insert("/data".to_string(), "/tmp/outside".to_string());

    let workspace = Path::new("/home/user/zeroclaw/workspace");
    let result = validate_workspace_paths("escape-plugin", &allowed, workspace);

    assert!(
        result.is_err(),
        "strict mode should reject a path outside the workspace root"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("escape-plugin"),
        "error should name the plugin: {err_msg}"
    );
    assert!(
        err_msg.contains("/tmp/outside"),
        "error should name the offending path: {err_msg}"
    );
    assert!(
        err_msg.contains("workspace"),
        "error should reference the workspace root: {err_msg}"
    );
}

#[test]
fn strict_rejects_parent_traversal_outside_workspace() {
    // Use a real temp directory so canonicalization can resolve `..` properly.
    let workspace = std::env::temp_dir().join("zeroclaw_test_strict_traversal");
    std::fs::create_dir_all(&workspace).ok();

    let mut allowed = HashMap::new();
    // Relative path that escapes via .. — resolves outside the workspace.
    allowed.insert("/data".to_string(), "../../etc/passwd".to_string());

    let result = validate_workspace_paths("traversal-plugin", &allowed, &workspace);

    assert!(
        result.is_err(),
        "strict mode should reject paths that escape the workspace via .."
    );

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn strict_allows_relative_path_inside_workspace() {
    // Use a real temporary directory so canonicalization works
    let workspace = std::env::temp_dir().join("zeroclaw_test_strict_workspace");
    let sub = workspace.join("plugins").join("data");
    std::fs::create_dir_all(&sub).ok();

    let mut allowed = HashMap::new();
    allowed.insert("/data".to_string(), "plugins/data".to_string());

    let result = validate_workspace_paths("inner-plugin", &allowed, &workspace);

    assert!(
        result.is_ok(),
        "strict mode should allow a relative path that resolves inside the workspace, but got: {:?}",
        result.unwrap_err()
    );

    // Clean up
    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn strict_allows_absolute_path_inside_workspace() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_strict_abs");
    let inner = workspace.join("data");
    std::fs::create_dir_all(&inner).ok();

    let mut allowed = HashMap::new();
    allowed.insert("/data".to_string(), inner.to_string_lossy().into_owned());

    let result = validate_workspace_paths("abs-inner-plugin", &allowed, &workspace);

    assert!(
        result.is_ok(),
        "strict mode should allow an absolute path inside the workspace, but got: {:?}",
        result.unwrap_err()
    );

    std::fs::remove_dir_all(&workspace).ok();
}

#[test]
fn strict_rejects_root_path() {
    let mut allowed = HashMap::new();
    allowed.insert("/mnt".to_string(), "/".to_string());

    let workspace = Path::new("/home/user/zeroclaw/workspace");
    let result = validate_workspace_paths("root-plugin", &allowed, workspace);

    assert!(
        result.is_err(),
        "strict mode should reject '/' as an allowed path"
    );
}

// ── Config integration ─────────────────────────────────────────────────

#[test]
fn strict_config_string_maps_to_strict_level() {
    let level = NetworkSecurityLevel::from_config("strict");
    assert_eq!(level, NetworkSecurityLevel::Strict);
}

#[test]
fn strict_config_is_case_insensitive() {
    assert_eq!(
        NetworkSecurityLevel::from_config("STRICT"),
        NetworkSecurityLevel::Strict
    );
    assert_eq!(
        NetworkSecurityLevel::from_config("Strict"),
        NetworkSecurityLevel::Strict
    );
}

#[test]
fn strict_config_round_trips_through_toml() {
    let toml_str = r#"
        signature_mode = "disabled"
        network_security_level = "strict"
    "#;

    let config: zeroclaw::config::schema::PluginSecurityConfig =
        toml::from_str(toml_str).expect("should parse plugin security config");

    assert_eq!(config.network_security_level, "strict");
    assert_eq!(
        NetworkSecurityLevel::from_config(&config.network_security_level),
        NetworkSecurityLevel::Strict,
    );
}
