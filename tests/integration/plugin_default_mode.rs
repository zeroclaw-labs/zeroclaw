//! Security test: default mode allows capabilities but warns on wildcards.
//!
//! Verifies that `validate_allowed_hosts` allows wildcard hosts at the Default
//! security level (while strict/paranoid would reject them), and that the
//! default level does not enforce the plugin allowlist or workspace path
//! restrictions.

use zeroclaw::plugins::loader::{
    validate_allowed_hosts, validate_plugin_allowlist, NetworkSecurityLevel,
};

// ── Default mode allows wildcard hosts ─────────────────────────────

#[test]
fn default_allows_wildcard_star_host() {
    let hosts = vec!["*.example.com".to_string()];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Default);

    assert!(
        result.is_ok(),
        "default mode should allow wildcard hosts, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn default_allows_bare_star_host() {
    let hosts = vec!["*".to_string()];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Default);

    assert!(
        result.is_ok(),
        "default mode should allow bare * host, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn default_allows_multiple_wildcards_among_hosts() {
    let hosts = vec![
        "api.example.com".to_string(),
        "*.internal.dev".to_string(),
        "*".to_string(),
    ];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Default);

    assert!(
        result.is_ok(),
        "default mode should allow all hosts including wildcards, but got: {:?}",
        result.unwrap_err()
    );
}

// ── Default mode allows explicit hosts (same as all levels) ────────

#[test]
fn default_allows_explicit_hosts() {
    let hosts = vec!["api.example.com".to_string(), "cdn.example.com".to_string()];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Default);

    assert!(
        result.is_ok(),
        "default mode should allow explicit hosts, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn default_allows_empty_hosts_list() {
    let hosts: Vec<String> = vec![];
    let result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Default);

    assert!(
        result.is_ok(),
        "default mode should allow an empty hosts list"
    );
}

// ── Contrast: strict/paranoid would reject the same wildcards ──────

#[test]
fn strict_rejects_same_wildcard_that_default_allows() {
    let hosts = vec!["*.example.com".to_string()];

    let default_result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Default);
    let strict_result = validate_allowed_hosts("my-plugin", &hosts, NetworkSecurityLevel::Strict);

    assert!(default_result.is_ok(), "default should allow wildcard");
    assert!(
        strict_result.is_err(),
        "strict should reject the same wildcard that default allows"
    );
}

// ── Default mode does not enforce the plugin allowlist ──────────────

#[test]
fn default_does_not_enforce_allowlist() {
    let allowed: Vec<String> = vec!["other-plugin".to_string()];
    let result =
        validate_plugin_allowlist("unlisted-plugin", &allowed, NetworkSecurityLevel::Default);

    assert!(
        result.is_ok(),
        "default mode should not enforce the plugin allowlist"
    );
}

#[test]
fn default_does_not_enforce_empty_allowlist() {
    let allowed: Vec<String> = vec![];
    let result = validate_plugin_allowlist("any-plugin", &allowed, NetworkSecurityLevel::Default);

    assert!(
        result.is_ok(),
        "default mode with empty allowlist should still allow plugins"
    );
}

// ── Config integration ─────────────────────────────────────────────

#[test]
fn default_config_string_maps_to_default_level() {
    let level = NetworkSecurityLevel::from_config("default");
    assert_eq!(level, NetworkSecurityLevel::Default);
}

#[test]
fn unknown_config_string_falls_back_to_default() {
    let level = NetworkSecurityLevel::from_config("unknown-value");
    assert_eq!(level, NetworkSecurityLevel::Default);
}

#[test]
fn empty_config_string_falls_back_to_default() {
    let level = NetworkSecurityLevel::from_config("");
    assert_eq!(level, NetworkSecurityLevel::Default);
}

#[test]
fn default_config_round_trips_through_toml() {
    let toml_str = r#"
        signature_mode = "disabled"
        network_security_level = "default"
    "#;

    let config: zeroclaw::config::schema::PluginSecurityConfig =
        toml::from_str(toml_str).expect("should parse plugin security config");

    assert_eq!(config.network_security_level, "default");

    let level = NetworkSecurityLevel::from_config(&config.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Default);
}
