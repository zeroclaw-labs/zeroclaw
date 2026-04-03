#![cfg(feature = "plugins-wasm")]

//! Security test: relaxed mode allows all declared capabilities.
//!
//! Verifies that `validate_allowed_hosts` allows wildcard hosts at the Relaxed
//! security level without producing warnings, and that the relaxed level does
//! not enforce the plugin allowlist or workspace path restrictions.

use zeroclaw::plugins::loader::{
    NetworkSecurityLevel, validate_allowed_hosts, validate_plugin_allowlist,
};

// ── Relaxed mode allows wildcard hosts ──────────────────────────────

#[test]
fn relaxed_allows_wildcard_star_host() {
    let hosts = vec!["*.example.com".to_string()];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Relaxed);

    assert!(
        result.is_ok(),
        "relaxed mode should allow wildcard hosts, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn relaxed_allows_bare_star_host() {
    let hosts = vec!["*".to_string()];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Relaxed);

    assert!(
        result.is_ok(),
        "relaxed mode should allow bare * host, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn relaxed_allows_multiple_wildcards_among_hosts() {
    let hosts = vec![
        "api.example.com".to_string(),
        "*.internal.dev".to_string(),
        "*".to_string(),
    ];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Relaxed);

    assert!(
        result.is_ok(),
        "relaxed mode should allow all hosts including wildcards, but got: {:?}",
        result.unwrap_err()
    );
}

// ── Relaxed mode allows explicit hosts (same as all levels) ─────────

#[test]
fn relaxed_allows_explicit_hosts() {
    let hosts = vec!["api.example.com".to_string(), "cdn.example.com".to_string()];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Relaxed);

    assert!(
        result.is_ok(),
        "relaxed mode should allow explicit hosts, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn relaxed_allows_empty_hosts_list() {
    let hosts: Vec<String> = vec![];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Relaxed);

    assert!(
        result.is_ok(),
        "relaxed mode should allow an empty hosts list"
    );
}

// ── Contrast: strict/paranoid reject what relaxed allows ────────────

#[test]
fn strict_rejects_same_wildcard_that_relaxed_allows() {
    let hosts = vec!["*.example.com".to_string()];

    let relaxed_result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Relaxed);
    let strict_result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Strict);

    assert!(relaxed_result.is_ok(), "relaxed should allow wildcard");
    assert!(
        strict_result.is_err(),
        "strict should reject the same wildcard that relaxed allows"
    );
}

#[test]
fn paranoid_rejects_same_wildcard_that_relaxed_allows() {
    let hosts = vec!["*".to_string()];

    let relaxed_result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Relaxed);
    let paranoid_result =
        validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Paranoid);

    assert!(relaxed_result.is_ok(), "relaxed should allow bare wildcard");
    assert!(
        paranoid_result.is_err(),
        "paranoid should reject the same wildcard that relaxed allows"
    );
}

// ── Relaxed mode does not enforce the plugin allowlist ───────────────

#[test]
fn relaxed_does_not_enforce_allowlist() {
    let allowed: Vec<String> = vec!["other-plugin".to_string()];
    let result =
        validate_plugin_allowlist("unlisted-plugin", &allowed, NetworkSecurityLevel::Relaxed);

    assert!(
        result.is_ok(),
        "relaxed mode should not enforce the plugin allowlist"
    );
}

#[test]
fn relaxed_does_not_enforce_empty_allowlist() {
    let allowed: Vec<String> = vec![];
    let result = validate_plugin_allowlist("any-plugin", &allowed, NetworkSecurityLevel::Relaxed);

    assert!(
        result.is_ok(),
        "relaxed mode with empty allowlist should still allow plugins"
    );
}

// ── Config integration ──────────────────────────────────────────────

#[test]
fn relaxed_config_string_maps_to_relaxed_level() {
    let level = NetworkSecurityLevel::from_config("relaxed");
    assert_eq!(level, NetworkSecurityLevel::Relaxed);
}

#[test]
fn relaxed_config_string_case_insensitive() {
    assert_eq!(
        NetworkSecurityLevel::from_config("Relaxed"),
        NetworkSecurityLevel::Relaxed
    );
    assert_eq!(
        NetworkSecurityLevel::from_config("RELAXED"),
        NetworkSecurityLevel::Relaxed
    );
    assert_eq!(
        NetworkSecurityLevel::from_config("rElAxEd"),
        NetworkSecurityLevel::Relaxed
    );
}

#[test]
fn relaxed_config_round_trips_through_toml() {
    let toml_str = r#"
        signature_mode = "disabled"
        network_security_level = "relaxed"
    "#;

    let config: zeroclaw::config::schema::PluginSecurityConfig =
        toml::from_str(toml_str).expect("should parse plugin security config");

    assert_eq!(config.network_security_level, "relaxed");

    let level = NetworkSecurityLevel::from_config(&config.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Relaxed);
}
