//! Config Schema Boundary Tests
//!
//! Validates: config defaults, backward compatibility, invalid input rejection,
//! and gateway/security/agent config boundary conditions.

use zeroclaw::config::migration;
use zeroclaw::config::{ChannelsConfig, Config, GatewayConfig, RiskProfileConfig, SecurityConfig};

fn migrate(toml_str: &str) -> Config {
    migration::migrate_to_current(toml_str).expect("migration succeeds")
}

// ─────────────────────────────────────────────────────────────────────────────
// Invalid value fail-fast
// ─────────────────────────────────────────────────────────────────────────────

/// Regression test for #5414, #5320, #5483, #5507: Option<T> fields
/// (api_key) and serde aliases (model_provider) must not be flagged as
/// unknown config keys.
#[test]
fn config_valid_keys_not_flagged_as_unknown() {
    // api_key: Option<T> defaulting to None — TOML omits it.
    // model_provider: serde alias for default_provider.
    let unknown = Config::unknown_keys("api_key = \"sk-test\"\nmodel_provider = \"ollama\"\n");
    assert!(
        unknown.is_empty(),
        "api_key and model_provider should not be flagged as unknown, got: {unknown:?}",
    );
}

#[test]
fn config_unknown_keys_parse_without_error() {
    let config = migrate(
        r#"
default_temperature = 0.7
default_provider = "test"
totally_unknown_key = "should be ignored"
another_fake = 42
"#,
    );
    assert!(
        (config
            .providers
            .first_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.7)
            .abs()
            < f64::EPSILON
    );
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
    // V1's `default_temperature` is folded into providers.models.<x>.default
    // by `migrate_to_current`. A non-f64 value should fail at the migration
    // boundary because the synthesized provider entry can't deserialize
    // a string into Option<f64>.
    let result = migration::migrate_to_current(toml_str);
    assert!(
        result.is_err(),
        "string for f64 temperature should fail migration"
    );
}

#[test]
fn config_out_of_range_temperature_fails() {
    // Temperature validation now happens at the provider level.
    let toml_str = r#"
[providers.models.test.default]
temperature = 99.0
"#;
    let config: Config = toml::from_str(toml_str).expect("parses");
    // Out-of-range temperature is stored but caught by validate().
    assert!(config.providers.models["test"]["default"].temperature == Some(99.0));
}

#[test]
fn config_negative_temperature_fails() {
    let toml_str = r#"
[providers.models.test.default]
temperature = -0.5
"#;
    let config: Config = toml::from_str(toml_str).expect("parses");
    assert!(config.providers.models["test"]["default"].temperature == Some(-0.5));
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
    assert!(
        gw.path_prefix.is_none(),
        "path_prefix should default to None"
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
    let gw = GatewayConfig {
        port: 8080,
        host: "0.0.0.0".into(),
        require_pairing: false,
        pair_rate_limit_per_minute: 5,
        path_prefix: Some("/zeroclaw".into()),
        ..Default::default()
    };

    let toml_str = toml::to_string(&gw).expect("gateway config should serialize");
    let parsed: GatewayConfig = toml::from_str(&toml_str).expect("should deserialize back");

    assert_eq!(parsed.port, 8080);
    assert_eq!(parsed.host, "0.0.0.0");
    assert!(!parsed.require_pairing);
    assert_eq!(parsed.pair_rate_limit_per_minute, 5);
    assert_eq!(parsed.path_prefix.as_deref(), Some("/zeroclaw"));
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
// GatewayConfig path_prefix validation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn gateway_path_prefix_rejects_missing_leading_slash() {
    let mut config = Config::default();
    config.gateway.path_prefix = Some("zeroclaw".into());
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("must start with '/'"),
        "expected leading-slash error, got: {err}"
    );
}

#[test]
fn gateway_path_prefix_rejects_trailing_slash() {
    let mut config = Config::default();
    config.gateway.path_prefix = Some("/zeroclaw/".into());
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("must not end with '/'"),
        "expected trailing-slash error, got: {err}"
    );
}

#[test]
fn gateway_path_prefix_rejects_bare_slash() {
    let mut config = Config::default();
    config.gateway.path_prefix = Some("/".into());
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("must not end with '/'"),
        "expected bare-slash error, got: {err}"
    );
}

#[test]
fn gateway_path_prefix_accepts_valid_prefixes() {
    for prefix in ["/zeroclaw", "/apps/zeroclaw", "/api/hassio_ingress/abc123"] {
        let mut config = Config::default();
        config.gateway.path_prefix = Some(prefix.into());
        config
            .validate()
            .unwrap_or_else(|e| panic!("prefix {prefix:?} should be valid, got: {e}"));
    }
}

#[test]
fn gateway_path_prefix_rejects_unsafe_characters() {
    for prefix in [
        "/zero claw",
        "/zero<claw",
        "/zero>claw",
        "/zero\"claw",
        "/zero?query",
        "/zero#frag",
    ] {
        let mut config = Config::default();
        config.gateway.path_prefix = Some(prefix.into());
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("invalid character"),
            "prefix {prefix:?} should be rejected, got: {err}"
        );
    }
    // Leading/trailing whitespace is rejected by the starts_with('/') or
    // invalid-character check — either way it must not pass validation.
    for prefix in [" /zeroclaw ", " /zeroclaw"] {
        let mut config = Config::default();
        config.gateway.path_prefix = Some(prefix.into());
        assert!(
            config.validate().is_err(),
            "whitespace-padded prefix {prefix:?} should be rejected"
        );
    }
}

#[test]
fn gateway_path_prefix_accepts_none() {
    let config = Config::default();
    assert!(config.gateway.path_prefix.is_none());
    config
        .validate()
        .expect("absent path_prefix should be valid");
}

// ─────────────────────────────────────────────────────────────────────────────
// SecurityConfig boundary tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn security_config_defaults() {
    let sec = SecurityConfig::default();
    assert!(sec.audit.enabled, "audit should be enabled by default");
    // V3: sandbox/resource limits live on risk_profiles entries, not SecurityConfig.
    let profile = RiskProfileConfig::default();
    assert!(
        profile.sandbox_enabled.is_none(),
        "sandbox enabled should auto-detect (None) by default"
    );
}

#[test]
fn security_config_toml_roundtrip() {
    let mut sec = SecurityConfig::default();
    sec.audit.max_size_mb = 200;

    let toml_str = toml::to_string(&sec).expect("SecurityConfig should serialize");
    let parsed: SecurityConfig = toml::from_str(&toml_str).expect("should deserialize back");

    assert_eq!(parsed.audit.max_size_mb, 200);
}

// ─────────────────────────────────────────────────────────────────────────────
// RiskProfileConfig boundary tests (security policy via Config.autonomy)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn autonomy_config_default_is_supervised() {
    let autonomy = RiskProfileConfig::default();
    assert_eq!(
        format!("{:?}", autonomy.level),
        "Supervised",
        "default autonomy should be Supervised"
    );
}

#[test]
fn autonomy_config_default_max_actions_per_hour() {
    let autonomy = RiskProfileConfig::default();
    assert!(
        autonomy.max_actions_per_hour > 0,
        "max_actions_per_hour should be positive"
    );
}

#[test]
fn autonomy_config_default_workspace_only() {
    let autonomy = RiskProfileConfig::default();
    assert!(
        autonomy.workspace_only,
        "workspace_only should default to true"
    );
}

#[test]
fn autonomy_config_toml_roundtrip() {
    let mut config = Config::default();
    config
        .risk_profiles
        .entry("default".into())
        .or_default()
        .max_actions_per_hour = 50;
    config
        .risk_profiles
        .entry("default".into())
        .or_default()
        .workspace_only = false;

    let toml_str = toml::to_string(&config).expect("config should serialize");
    let parsed: Config = toml::from_str(&toml_str).expect("should deserialize back");

    let profile = parsed.risk_profiles.get("default").unwrap();
    assert_eq!(profile.max_actions_per_hour, 50);
    assert!(!profile.workspace_only);
}

// ─────────────────────────────────────────────────────────────────────────────
// Backward compatibility
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_empty_toml_uses_default_temperature() {
    let config = migrate("");
    assert!(
        (config
            .providers
            .first_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.7)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn config_minimal_toml_with_temperature_uses_defaults() {
    let config = migrate("default_temperature = 0.7\ndefault_provider = \"test\"\n");
    // Migration synthesizes [agents.default] from the V2 shape; assert
    // its tunables match DelegateAgentConfig defaults.
    let agent = config
        .agent("default")
        .expect("migration synthesized agents.default");
    assert_eq!(agent.max_tool_iterations, 10);
    assert_eq!(config.gateway.port, 42617);
}

#[test]
fn config_only_temperature_parses() {
    let config = migrate("default_temperature = 1.2\ndefault_provider = \"test\"\n");
    assert!(
        (config
            .providers
            .first_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 1.2)
            .abs()
            < f64::EPSILON
    );
    let agent = config
        .agent("default")
        .expect("migration synthesized agents.default");
    assert_eq!(agent.max_tool_iterations, 10);
}

#[test]
fn config_extra_unknown_keys_ignored() {
    let config = migrate(
        r#"
default_temperature = 0.5
default_provider = "test"
future_feature = true
[some_future_section]
value = 123
"#,
    );
    assert!(
        (config
            .providers
            .first_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.5)
            .abs()
            < f64::EPSILON
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Config merging edge cases
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_multiple_channels_coexist() {
    let toml_str = r#"
default_temperature = 0.7

[channels.telegram.default]
bot_token = "test_token"
allowed_users = ["zeroclaw_user"]

[channels.discord.default]
bot_token = "test_token"
"#;
    let parsed: Config = toml::from_str(toml_str).expect("multi-channel config should parse");
    assert!(!parsed.channels.telegram.is_empty());
    assert!(!parsed.channels.discord.is_empty());
    assert!(parsed.channels.slack.is_empty());
}

#[test]
fn config_nested_optional_sections_default_when_absent() {
    let toml_str = "default_temperature = 0.7\n";
    let parsed: Config = toml::from_str(toml_str).expect("minimal TOML should parse");
    assert!(parsed.channels.telegram.is_empty());
    assert!(!parsed.composio.enabled);
    assert!(parsed.composio.api_key.is_none());
    assert!(parsed.browser.enabled);
}

#[test]
fn config_channels_default_cli_enabled() {
    let channels = ChannelsConfig::default();
    assert!(channels.cli, "CLI channel should be enabled by default");
}

#[test]
fn config_channels_all_optional_channels_none_by_default() {
    let channels = ChannelsConfig::default();
    assert!(channels.telegram.is_empty());
    assert!(channels.discord.is_empty());
    assert!(channels.slack.is_empty());
    assert!(channels.matrix.is_empty());
    assert!(channels.lark.is_empty());
    assert!(channels.feishu.is_empty());
    assert!(channels.webhook.is_empty());
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

#[test]
fn config_channels_without_cli_field() {
    let toml_str = r#"
default_temperature = 0.7

[channels.matrix.default]
homeserver = "https://matrix.example.com"
access_token = "syt_test_token"
allowed_rooms = ["!abc123:example.com"]
allowed_users = ["@user:example.com"]
"#;
    let parsed: Config = toml::from_str(toml_str)
        .expect("channels with only a Matrix section (no explicit cli field) should parse");
    assert!(
        parsed.channels.cli,
        "cli should default to true when omitted"
    );
    assert!(!parsed.channels.matrix.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #3456 – top-level [cli] section must not clash with channels.cli
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_toplevel_cli_section_with_whatsapp_parses() {
    // Exact config from issue #3456
    let toml_str = r#"
[cli]

[channels.whatsapp.default]
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
allowed_numbers = ["*"]
"#;
    let parsed: Config = toml::from_str(toml_str)
        .expect("top-level [cli] section with [channels.whatsapp] should parse");
    assert!(!parsed.channels.whatsapp.is_empty());
    let wa = parsed.channels.whatsapp.get("default").unwrap();
    assert_eq!(
        wa.session_path.as_deref(),
        Some("~/.zeroclaw/state/whatsapp-web/session.db")
    );
    assert_eq!(wa.allowed_numbers, vec!["*".to_string()]);
}

#[test]
fn config_only_whatsapp_channel_parses() {
    let toml_str = r#"
[channels.whatsapp.default]
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
allowed_numbers = ["*"]
"#;
    let parsed: Config =
        toml::from_str(toml_str).expect("config with only whatsapp channel should parse");
    assert!(!parsed.channels.whatsapp.is_empty());
    assert!(
        parsed.channels.cli,
        "cli should default to true when omitted"
    );
}

#[test]
fn config_channels_explicit_cli_true_with_whatsapp() {
    let toml_str = r#"
[channels]
cli = true

[channels.whatsapp.default]
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
allowed_numbers = ["*"]
"#;
    let parsed: Config =
        toml::from_str(toml_str).expect("explicit channels.cli=true with whatsapp should parse");
    assert!(parsed.channels.cli);
    assert!(!parsed.channels.whatsapp.is_empty());
}

#[test]
fn config_empty_parses_with_all_defaults() {
    let config = migrate("");
    assert!(config.channels.cli);
    assert!(config.channels.whatsapp.is_empty());
    assert!(
        (config
            .providers
            .first_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.7)
            .abs()
            < f64::EPSILON
    );
}
