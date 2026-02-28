#![allow(clippy::field_reassign_with_default)]
//! Config Schema Boundary Tests
//!
//! Validates: config defaults, backward compatibility, invalid input rejection,
//! and gateway/security/agent config boundary conditions.

use zeroclaw::config::{AutonomyConfig, ChannelsConfig, Config, GatewayConfig, SecurityConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Invalid value fail-fast
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_unknown_keys_parse_without_error() {
    let toml_str = r#"
default_temperature = 0.7
totally_unknown_key = "should be ignored"
another_fake = 42
"#;
    let parsed: Config = toml::from_str(toml_str).expect("unknown keys should be ignored");
    assert!((parsed.default_temperature - 0.7).abs() < f64::EPSILON);
}

#[test]
fn config_wrong_type_for_port_fails() {
    let toml_str = r#"
[gateway]
port = "not_a_number"
"#;
    let result: Result<Config, _> = toml::from_str(toml_str);
    assert!(result.is_err(), "string for u16 port should fail to parse");
}

#[test]
fn config_wrong_type_for_temperature_fails() {
    let toml_str = r#"
default_temperature = "hot"
"#;
    let result: Result<Config, _> = toml::from_str(toml_str);
    assert!(
        result.is_err(),
        "string for f64 temperature should fail to parse"
    );
}

#[test]
fn config_negative_port_fails() {
    let toml_str = r#"
[gateway]
port = -1
"#;
    let result: Result<Config, _> = toml::from_str(toml_str);
    assert!(result.is_err(), "negative port should fail for u16");
}

#[test]
fn config_overflow_port_fails() {
    let toml_str = r#"
[gateway]
port = 99999
"#;
    let result: Result<Config, _> = toml::from_str(toml_str);
    assert!(result.is_err(), "port > 65535 should fail for u16");
}

// ─────────────────────────────────────────────────────────────────────────────
// GatewayConfig boundary tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn gateway_config_defaults_are_secure() {
    let gw = GatewayConfig::default();
    assert_eq!(gw.port, 42617);
    assert_eq!(gw.host, "127.0.0.1");
    assert!(gw.require_pairing, "pairing should be required by default");
    assert!(
        !gw.allow_public_bind,
        "public bind should be denied by default"
    );
    assert!(
        !gw.trust_forwarded_headers,
        "forwarded headers should be untrusted by default"
    );
}

#[test]
fn gateway_config_rate_limit_defaults() {
    let gw = GatewayConfig::default();
    assert_eq!(gw.pair_rate_limit_per_minute, 10);
    assert_eq!(gw.webhook_rate_limit_per_minute, 60);
    assert_eq!(gw.rate_limit_max_keys, 10_000);
}

#[test]
fn gateway_config_idempotency_defaults() {
    let gw = GatewayConfig::default();
    assert_eq!(gw.idempotency_ttl_secs, 300);
    assert_eq!(gw.idempotency_max_keys, 10_000);
}

#[test]
fn gateway_config_toml_roundtrip() {
    let mut gw = GatewayConfig::default();
    gw.port = 8080;
    gw.host = "0.0.0.0".into();
    gw.require_pairing = false;
    gw.pair_rate_limit_per_minute = 5;

    let toml_str = toml::to_string(&gw).expect("gateway config should serialize");
    let parsed: GatewayConfig = toml::from_str(&toml_str).expect("should deserialize back");

    assert_eq!(parsed.port, 8080);
    assert_eq!(parsed.host, "0.0.0.0");
    assert!(!parsed.require_pairing);
    assert_eq!(parsed.pair_rate_limit_per_minute, 5);
}

#[test]
fn gateway_config_missing_section_uses_defaults() {
    let toml_str = r#"
default_temperature = 0.5
"#;
    let parsed: Config = toml::from_str(toml_str).expect("missing gateway section should parse");
    assert_eq!(parsed.gateway.port, 42617);
    assert_eq!(parsed.gateway.host, "127.0.0.1");
    assert!(parsed.gateway.require_pairing);
    assert!(!parsed.gateway.allow_public_bind);
}

#[test]
fn gateway_config_partial_section_fills_defaults() {
    let toml_str = r#"
default_temperature = 0.7

[gateway]
port = 9090
"#;
    let parsed: Config = toml::from_str(toml_str).expect("partial gateway should parse");
    assert_eq!(parsed.gateway.port, 9090);
    assert_eq!(parsed.gateway.host, "127.0.0.1");
    assert!(parsed.gateway.require_pairing);
    assert_eq!(parsed.gateway.pair_rate_limit_per_minute, 10);
}

// ─────────────────────────────────────────────────────────────────────────────
// SecurityConfig boundary tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn security_config_defaults() {
    let sec = SecurityConfig::default();
    assert!(
        sec.sandbox.enabled.is_none(),
        "sandbox enabled should auto-detect (None) by default"
    );
    assert!(sec.audit.enabled, "audit should be enabled by default");
}

#[test]
fn security_config_toml_roundtrip() {
    let mut sec = SecurityConfig::default();
    sec.sandbox.enabled = Some(true);
    sec.audit.max_size_mb = 200;

    let toml_str = toml::to_string(&sec).expect("SecurityConfig should serialize");
    let parsed: SecurityConfig = toml::from_str(&toml_str).expect("should deserialize back");

    assert_eq!(parsed.sandbox.enabled, Some(true));
    assert_eq!(parsed.audit.max_size_mb, 200);
}

// ─────────────────────────────────────────────────────────────────────────────
// AutonomyConfig boundary tests (security policy via Config.autonomy)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn autonomy_config_default_is_supervised() {
    let autonomy = AutonomyConfig::default();
    assert_eq!(
        format!("{:?}", autonomy.level),
        "Supervised",
        "default autonomy should be Supervised"
    );
}

#[test]
fn autonomy_config_default_max_actions_per_hour() {
    let autonomy = AutonomyConfig::default();
    assert!(
        autonomy.max_actions_per_hour > 0,
        "max_actions_per_hour should be positive"
    );
}

#[test]
fn autonomy_config_default_workspace_only() {
    let autonomy = AutonomyConfig::default();
    assert!(
        autonomy.workspace_only,
        "workspace_only should default to true"
    );
}

#[test]
fn autonomy_config_toml_roundtrip() {
    let mut config = Config::default();
    config.autonomy.max_actions_per_hour = 50;
    config.autonomy.workspace_only = false;

    let toml_str = toml::to_string(&config).expect("config should serialize");
    let parsed: Config = toml::from_str(&toml_str).expect("should deserialize back");

    assert_eq!(parsed.autonomy.max_actions_per_hour, 50);
    assert!(!parsed.autonomy.workspace_only);
}

// ─────────────────────────────────────────────────────────────────────────────
// Backward compatibility
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_empty_toml_requires_temperature() {
    let result: Result<Config, _> = toml::from_str("");
    assert!(
        result.is_err(),
        "empty TOML should fail because default_temperature is required"
    );
}

#[test]
fn config_minimal_toml_with_temperature_uses_defaults() {
    let toml_str = "default_temperature = 0.7\n";
    let parsed: Config = toml::from_str(toml_str).expect("minimal TOML should parse");
    assert_eq!(parsed.agent.max_tool_iterations, 20);
    assert_eq!(parsed.gateway.port, 42617);
}

#[test]
fn config_only_temperature_parses() {
    let toml_str = "default_temperature = 1.2\n";
    let parsed: Config = toml::from_str(toml_str).expect("temperature-only TOML should parse");
    assert!((parsed.default_temperature - 1.2).abs() < f64::EPSILON);
    assert_eq!(parsed.agent.max_tool_iterations, 20);
}

#[test]
fn config_extra_unknown_keys_ignored() {
    let toml_str = r#"
default_temperature = 0.5
future_feature = true
[some_future_section]
value = 123
"#;
    let parsed: Config =
        toml::from_str(toml_str).expect("unknown keys and sections should be ignored");
    assert!((parsed.default_temperature - 0.5).abs() < f64::EPSILON);
}

// ─────────────────────────────────────────────────────────────────────────────
// Config merging edge cases
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_multiple_channels_coexist() {
    let toml_str = r#"
default_temperature = 0.7

[channels_config]
cli = true

[channels_config.telegram]
bot_token = "test_token"
allowed_users = ["zeroclaw_user"]

[channels_config.discord]
bot_token = "test_token"
"#;
    let parsed: Config = toml::from_str(toml_str).expect("multi-channel config should parse");
    assert!(parsed.channels_config.telegram.is_some());
    assert!(parsed.channels_config.discord.is_some());
    assert!(parsed.channels_config.slack.is_none());
}

#[test]
fn config_nested_optional_sections_default_when_absent() {
    let toml_str = "default_temperature = 0.7\n";
    let parsed: Config = toml::from_str(toml_str).expect("minimal TOML should parse");
    assert!(parsed.channels_config.telegram.is_none());
    assert!(!parsed.composio.enabled);
    assert!(parsed.composio.api_key.is_none());
    assert!(!parsed.browser.enabled);
}

#[test]
fn config_channels_default_cli_enabled() {
    let channels = ChannelsConfig::default();
    assert!(channels.cli, "CLI channel should be enabled by default");
}

#[test]
fn config_channels_all_optional_channels_none_by_default() {
    let channels = ChannelsConfig::default();
    assert!(channels.telegram.is_none());
    assert!(channels.discord.is_none());
    assert!(channels.slack.is_none());
    assert!(channels.matrix.is_none());
    assert!(channels.lark.is_none());
    assert!(channels.feishu.is_none());
    assert!(channels.webhook.is_none());
}

#[test]
fn config_memory_defaults_when_section_absent() {
    let toml_str = "default_temperature = 0.7\n";
    let parsed: Config = toml::from_str(toml_str).expect("minimal TOML should parse");
    let mem = &parsed.memory;
    assert!(!mem.backend.is_empty());
    assert!(!mem.embedding_provider.is_empty());
    let weight_sum = mem.vector_weight + mem.keyword_weight;
    assert!(
        (weight_sum - 1.0).abs() < 0.01,
        "vector + keyword weights should sum to ~1.0"
    );
}
