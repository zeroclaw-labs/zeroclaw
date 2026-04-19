#![cfg(feature = "plugins-wasm")]

//! Security test: security level is read from SecurityPolicy configuration.
//!
//! Verifies that `NetworkSecurityLevel::from_config` correctly maps configuration
//! strings to the expected security levels, that `PluginSecurityConfig` deserialises
//! from TOML with the correct defaults, and that unknown values fall back safely.

use zeroclaw::config::schema::PluginSecurityConfig;
use zeroclaw::plugins::loader::NetworkSecurityLevel;

// ── from_config maps each level string correctly ─────────────────────

#[test]
fn from_config_maps_relaxed() {
    assert_eq!(
        NetworkSecurityLevel::from_config("relaxed"),
        NetworkSecurityLevel::Relaxed
    );
}

#[test]
fn from_config_maps_default() {
    assert_eq!(
        NetworkSecurityLevel::from_config("default"),
        NetworkSecurityLevel::Default
    );
}

#[test]
fn from_config_maps_strict() {
    assert_eq!(
        NetworkSecurityLevel::from_config("strict"),
        NetworkSecurityLevel::Strict
    );
}

#[test]
fn from_config_maps_paranoid() {
    assert_eq!(
        NetworkSecurityLevel::from_config("paranoid"),
        NetworkSecurityLevel::Paranoid
    );
}

// ── Case-insensitive parsing ─────────────────────────────────────────

#[test]
fn from_config_is_case_insensitive() {
    assert_eq!(
        NetworkSecurityLevel::from_config("STRICT"),
        NetworkSecurityLevel::Strict
    );
    assert_eq!(
        NetworkSecurityLevel::from_config("Paranoid"),
        NetworkSecurityLevel::Paranoid
    );
    assert_eq!(
        NetworkSecurityLevel::from_config("Relaxed"),
        NetworkSecurityLevel::Relaxed
    );
    assert_eq!(
        NetworkSecurityLevel::from_config("DEFAULT"),
        NetworkSecurityLevel::Default
    );
}

// ── Unknown values fall back to Default ──────────────────────────────

#[test]
fn from_config_unknown_value_falls_back_to_default() {
    assert_eq!(
        NetworkSecurityLevel::from_config("unknown"),
        NetworkSecurityLevel::Default
    );
}

#[test]
fn from_config_empty_string_falls_back_to_default() {
    assert_eq!(
        NetworkSecurityLevel::from_config(""),
        NetworkSecurityLevel::Default
    );
}

#[test]
fn from_config_typo_falls_back_to_default() {
    assert_eq!(
        NetworkSecurityLevel::from_config("strct"),
        NetworkSecurityLevel::Default
    );
}

// ── TOML deserialization reads the configured level ──────────────────

#[test]
fn toml_strict_level_round_trips() {
    let toml_str = r#"
        signature_mode = "disabled"
        network_security_level = "strict"
    "#;

    let config: PluginSecurityConfig =
        toml::from_str(toml_str).expect("should parse plugin security config");

    assert_eq!(config.network_security_level, "strict");

    let level = NetworkSecurityLevel::from_config(&config.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Strict);
}

#[test]
fn toml_paranoid_level_round_trips() {
    let toml_str = r#"
        signature_mode = "disabled"
        network_security_level = "paranoid"
        allowed_plugins = ["trusted-plugin"]
    "#;

    let config: PluginSecurityConfig =
        toml::from_str(toml_str).expect("should parse plugin security config");

    assert_eq!(config.network_security_level, "paranoid");

    let level = NetworkSecurityLevel::from_config(&config.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Paranoid);
}

#[test]
fn toml_relaxed_level_round_trips() {
    let toml_str = r#"
        signature_mode = "disabled"
        network_security_level = "relaxed"
    "#;

    let config: PluginSecurityConfig =
        toml::from_str(toml_str).expect("should parse plugin security config");

    let level = NetworkSecurityLevel::from_config(&config.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Relaxed);
}

// ── Default config yields Default security level ─────────────────────

#[test]
fn default_plugin_security_config_has_default_level() {
    let config = PluginSecurityConfig::default();

    assert_eq!(config.network_security_level, "default");

    let level = NetworkSecurityLevel::from_config(&config.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Default);
}

#[test]
fn toml_missing_level_falls_back_to_default() {
    let toml_str = r#"
        signature_mode = "disabled"
    "#;

    let config: PluginSecurityConfig =
        toml::from_str(toml_str).expect("should parse with missing level");

    assert_eq!(config.network_security_level, "default");

    let level = NetworkSecurityLevel::from_config(&config.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Default);
}

// ── Full PluginsConfig reads nested security level ───────────────────

#[test]
fn full_plugins_config_reads_security_level() {
    let toml_str = r#"
        enabled = true
        plugins_dir = "/tmp/plugins"
        auto_discover = true
        max_plugins = 10

        [security]
        signature_mode = "disabled"
        network_security_level = "paranoid"
        allowed_plugins = ["my-plugin"]
    "#;

    let config: zeroclaw::config::schema::PluginsConfig =
        toml::from_str(toml_str).expect("should parse full plugins config");

    assert_eq!(config.security.network_security_level, "paranoid");

    let level = NetworkSecurityLevel::from_config(&config.security.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Paranoid);
}

#[test]
fn full_plugins_config_defaults_security_when_omitted() {
    let toml_str = r#"
        enabled = true
    "#;

    let config: zeroclaw::config::schema::PluginsConfig =
        toml::from_str(toml_str).expect("should parse with omitted security section");

    assert_eq!(config.security.network_security_level, "default");

    let level = NetworkSecurityLevel::from_config(&config.security.network_security_level);
    assert_eq!(level, NetworkSecurityLevel::Default);
}
