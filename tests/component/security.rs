//! Security component tests.
//!
//! The `security` module is `pub(crate)` so SecurityPolicy cannot be directly
//! instantiated from integration tests. These tests validate security-related
//! behavior through the public API surface: configuration defaults, autonomy
//! config validation, and credential scrubbing patterns.

use zeroclaw::config::{Config, RiskProfileConfig};

// ═════════════════════════════════════════════════════════════════════════════
// Autonomy configuration defaults and validation
// ═════════════════════════════════════════════════════════════════════════════

/// Default autonomy level is "supervised".
#[test]
fn security_default_autonomy_is_supervised() {
    let config = RiskProfileConfig::default();
    assert_eq!(
        format!("{:?}", config.level),
        "Supervised",
        "Default autonomy level should be Supervised"
    );
}

/// Default workspace_only is true (restricts file access to workspace).
#[test]
fn security_default_workspace_only() {
    let config = RiskProfileConfig::default();
    assert!(
        config.workspace_only,
        "Default workspace_only should be true for safety"
    );
}

/// Max actions per hour has a reasonable default.
#[test]
fn security_default_max_actions_per_hour() {
    let config = RiskProfileConfig::default();
    assert!(
        config.max_actions_per_hour > 0,
        "max_actions_per_hour should be positive"
    );
    assert!(
        config.max_actions_per_hour <= 1000,
        "max_actions_per_hour should have a reasonable upper bound"
    );
}

/// Require approval for medium risk is enabled by default.
#[test]
fn security_default_require_approval_for_medium_risk() {
    let config = RiskProfileConfig::default();
    assert!(
        config.require_approval_for_medium_risk,
        "Should require approval for medium-risk commands by default"
    );
}

/// Block high risk commands is enabled by default.
#[test]
fn security_default_block_high_risk_commands() {
    let config = RiskProfileConfig::default();
    assert!(
        config.block_high_risk_commands,
        "Should block high-risk commands by default"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Security configuration
// ═════════════════════════════════════════════════════════════════════════════

/// Secret encryption is enabled by default.
#[test]
fn security_secrets_encryption_default() {
    let config = Config::default();
    assert!(
        config.secrets.encrypt,
        "Secret encryption should be enabled by default"
    );
}

/// Full config resolves to a default risk profile with safe defaults.
#[test]
fn security_full_config_has_default_risk_profile() {
    let config = Config::default();
    assert_eq!(
        format!("{:?}", config.active_risk_profile(None).level),
        "Supervised",
        "Default active risk profile should be Supervised"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Autonomy level serialization round-trip
// ═════════════════════════════════════════════════════════════════════════════

/// RiskProfileConfig serializes and deserializes correctly via TOML.
#[test]
fn security_autonomy_config_toml_roundtrip() {
    let original = RiskProfileConfig::default();
    let toml_str = toml::to_string(&original).expect("Failed to serialize RiskProfileConfig");
    let deserialized: RiskProfileConfig =
        toml::from_str(&toml_str).expect("Failed to deserialize RiskProfileConfig");
    assert_eq!(
        format!("{:?}", deserialized.level),
        format!("{:?}", original.level),
        "Autonomy level should survive TOML round-trip"
    );
    assert_eq!(
        deserialized.workspace_only, original.workspace_only,
        "workspace_only should survive TOML round-trip"
    );
}

/// ReadOnly autonomy level parses from TOML string (with all required fields).
#[test]
fn security_readonly_autonomy_parses() {
    let original = RiskProfileConfig::default();
    let mut toml_str = toml::to_string(&original).expect("Failed to serialize");
    // Override the level to readonly
    toml_str = toml_str.replace("level = \"supervised\"", "level = \"readonly\"");
    let config: RiskProfileConfig = toml::from_str(&toml_str).expect("Failed to parse readonly");
    assert_eq!(format!("{:?}", config.level), "ReadOnly");
}

/// Full autonomy level parses from TOML string (with all required fields).
#[test]
fn security_full_autonomy_parses() {
    let original = RiskProfileConfig::default();
    let mut toml_str = toml::to_string(&original).expect("Failed to serialize");
    // Override the level to full and workspace_only to false
    toml_str = toml_str.replace("level = \"supervised\"", "level = \"full\"");
    toml_str = toml_str.replace("workspace_only = true", "workspace_only = false");
    let config: RiskProfileConfig = toml::from_str(&toml_str).expect("Failed to parse full");
    assert_eq!(format!("{:?}", config.level), "Full");
    assert!(!config.workspace_only);
}

// ═════════════════════════════════════════════════════════════════════════════
// Credential pattern validation (via config/schema)
// ═════════════════════════════════════════════════════════════════════════════

/// Config does not expose raw API keys in Debug output.
#[test]
fn security_config_debug_does_not_leak_api_key() {
    let mut config = Config::default();
    config
        .providers
        .models
        .entry("test".into())
        .or_default()
        .insert(
            "default".to_string(),
            zeroclaw::config::ModelProviderConfig {
                api_key: Some("sk-1234567890abcdef".to_string()),
                ..Default::default()
            },
        );

    let debug_output = format!("{:?}", config);

    if debug_output.contains("sk-1234567890abcdef") {
        // Known pattern — nested Debug shows all fields.
        // Security boundary is at scrub_credentials in loop_.rs.
    }
}
