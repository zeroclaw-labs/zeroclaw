//! Security component tests.

use zeroclaw::config::{Config, RiskProfileConfig};

// ═════════════════════════════════════════════════════════════════════════════
// Autonomy configuration defaults and validation
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn security_default_autonomy_is_supervised() {
    let config = RiskProfileConfig::default();
    assert_eq!(
        format!("{:?}", config.level),
        "Supervised",
        "Default autonomy level should be Supervised"
    );
}

#[test]
fn security_default_workspace_only() {
    let config = RiskProfileConfig::default();
    assert!(
        config.workspace_only,
        "Default workspace_only should be true for safety"
    );
}

#[test]
fn security_default_require_approval_for_medium_risk() {
    let config = RiskProfileConfig::default();
    assert!(
        config.require_approval_for_medium_risk,
        "Should require approval for medium-risk commands by default"
    );
}

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

#[test]
fn security_secrets_encryption_default() {
    let config = Config::default();
    assert!(
        config.secrets.encrypt,
        "Secret encryption should be enabled by default"
    );
}

#[test]
fn security_default_risk_profile_is_supervised() {
    let profile = RiskProfileConfig::default();
    assert_eq!(
        format!("{:?}", profile.level),
        "Supervised",
        "Default RiskProfileConfig autonomy level should be Supervised"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Autonomy level serialization round-trip
// ═════════════════════════════════════════════════════════════════════════════

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

#[test]
fn security_readonly_autonomy_parses() {
    let original = RiskProfileConfig::default();
    let mut toml_str = toml::to_string(&original).expect("Failed to serialize");
    // Override the level to readonly
    toml_str = toml_str.replace("level = \"supervised\"", "level = \"readonly\"");
    let config: RiskProfileConfig = toml::from_str(&toml_str).expect("Failed to parse readonly");
    assert_eq!(format!("{:?}", config.level), "ReadOnly");
}

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

#[test]
fn security_config_secret_property_readback_masks_api_key() {
    let mut config = Config::default();
    let path = "providers.models.openrouter.default.api_key";
    let secret = "sk-1234567890abcdef";

    assert!(
        Config::prop_is_secret(path),
        "{path} should be classified as a secret config property"
    );
    config
        .providers
        .models
        .ensure("openrouter", "default")
        .expect("openrouter provider entry should be creatable");
    config
        .set_prop(path, secret)
        .expect("secret config property should be settable");

    let readback = config
        .get_prop(path)
        .expect("secret config property should be readable");
    assert_ne!(
        readback, secret,
        "secret config property readback must not expose the raw API key"
    );
    assert!(
        readback.contains("****"),
        "secret config property readback should use a masked placeholder, got {readback:?}"
    );
}
