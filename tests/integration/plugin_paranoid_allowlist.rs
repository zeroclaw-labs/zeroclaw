#![cfg(feature = "plugins-wasm")]

//! Security test: paranoid mode rejects plugins not individually allowlisted.
//!
//! Verifies that `validate_plugin_allowlist` rejects any plugin whose name does
//! not appear in the `allowed_plugins` config list when running at the Paranoid
//! security level, and permits plugins that ARE on the list. At non-paranoid
//! levels, all plugins pass regardless of the allowlist.

use zeroclaw::plugins::loader::{NetworkSecurityLevel, validate_plugin_allowlist};

// ── Paranoid mode ────────────────────────────────────────────────────

#[test]
fn paranoid_rejects_plugin_not_on_allowlist() {
    let allowed: Vec<String> = vec!["trusted-plugin".to_string()];
    let result =
        validate_plugin_allowlist("rogue-plugin", &allowed, NetworkSecurityLevel::Paranoid);

    assert!(
        result.is_err(),
        "paranoid mode should reject a plugin not on the allowlist"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("rogue-plugin"),
        "error should name the rejected plugin: {err_msg}"
    );
    assert!(
        err_msg.contains("not allowlisted"),
        "error should mention allowlisting: {err_msg}"
    );
}

#[test]
fn paranoid_allows_plugin_on_allowlist() {
    let allowed: Vec<String> = vec!["trusted-plugin".to_string(), "another-trusted".to_string()];
    let result =
        validate_plugin_allowlist("trusted-plugin", &allowed, NetworkSecurityLevel::Paranoid);

    assert!(
        result.is_ok(),
        "paranoid mode should allow a plugin that is on the allowlist, but got: {:?}",
        result.unwrap_err()
    );
}

#[test]
fn paranoid_rejects_when_allowlist_is_empty() {
    let allowed: Vec<String> = vec![];
    let result = validate_plugin_allowlist("any-plugin", &allowed, NetworkSecurityLevel::Paranoid);

    assert!(
        result.is_err(),
        "paranoid mode with an empty allowlist should reject every plugin"
    );
}

// ── Non-paranoid levels pass unconditionally ─────────────────────────

#[test]
fn default_level_allows_any_plugin_regardless_of_allowlist() {
    let allowed: Vec<String> = vec!["other-plugin".to_string()];
    let result =
        validate_plugin_allowlist("unlisted-plugin", &allowed, NetworkSecurityLevel::Default);

    assert!(
        result.is_ok(),
        "default level should not enforce the allowlist"
    );
}

#[test]
fn strict_level_allows_any_plugin_regardless_of_allowlist() {
    let allowed: Vec<String> = vec![];
    let result = validate_plugin_allowlist("some-plugin", &allowed, NetworkSecurityLevel::Strict);

    assert!(
        result.is_ok(),
        "strict level should not enforce the allowlist"
    );
}

// ── Config integration ───────────────────────────────────────────────

#[test]
fn paranoid_config_string_maps_to_paranoid_level() {
    let level = NetworkSecurityLevel::from_config("paranoid");
    assert_eq!(level, NetworkSecurityLevel::Paranoid);
}

#[test]
fn allowed_plugins_config_round_trips_through_toml() {
    let toml_str = r#"
        signature_mode = "disabled"
        network_security_level = "paranoid"
        allowed_plugins = ["my-plugin", "other-plugin"]
    "#;

    let config: zeroclaw::config::schema::PluginSecurityConfig =
        toml::from_str(toml_str).expect("should parse plugin security config");

    assert_eq!(config.network_security_level, "paranoid");
    assert_eq!(config.allowed_plugins, vec!["my-plugin", "other-plugin"]);
}
