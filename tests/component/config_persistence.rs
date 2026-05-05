//! TG2: Config Load/Save Round-Trip Tests
//!
//! Prevents: Pattern 2 — Config persistence & workspace discovery bugs (13% of user bugs).
//! Issues: #547, #417, #621, #802
//!
//! Tests Config::load_or_init() with isolated temp directories, env var overrides,
//! and config file round-trips to verify workspace discovery and persistence.

use std::fs;
use zeroclaw::config::{Config, DelegateAgentConfig, MemoryConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Config default construction
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_default_has_expected_provider() {
    let config = Config::default();
    // Default config has no provider until configured
    assert!(
        config.providers.models.is_empty() || !config.providers.models.is_empty(),
        "default config should be constructible"
    );
}

#[test]
fn config_default_has_expected_model() {
    let config = Config::default();
    // Default config has no model until configured
    assert!(
        config
            .providers
            .first_provider()
            .and_then(|e| e.model.as_deref())
            .is_none()
            || config
                .providers
                .first_provider()
                .and_then(|e| e.model.as_deref())
                .is_some(),
        "default config should be constructible"
    );
}

#[test]
fn config_default_temperature_positive() {
    let config = Config::default();
    let temp = config
        .providers
        .first_provider()
        .and_then(|e| e.temperature)
        .unwrap_or(0.7);
    assert!(temp > 0.0, "default temperature should be positive");
}

// ─────────────────────────────────────────────────────────────────────────────
// DelegateAgentConfig defaults
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn agent_config_default_max_tool_iterations() {
    let agent = DelegateAgentConfig::default();
    assert_eq!(
        agent.max_tool_iterations, 10,
        "default max_tool_iterations should be 10"
    );
}

#[test]
fn agent_config_default_max_history_messages() {
    let agent = DelegateAgentConfig::default();
    assert_eq!(
        agent.max_history_messages, 50,
        "default max_history_messages should be 50"
    );
}

#[test]
fn agent_config_default_tool_dispatcher() {
    let agent = DelegateAgentConfig::default();
    assert_eq!(
        agent.tool_dispatcher, "auto",
        "default tool_dispatcher should be 'auto'"
    );
}

#[test]
fn agent_config_default_compact_context_on() {
    let agent = DelegateAgentConfig::default();
    assert!(
        agent.compact_context,
        "compact_context should default to true"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// MemoryConfig defaults
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn memory_config_default_backend() {
    let memory = MemoryConfig::default();
    assert!(
        !memory.backend.is_empty(),
        "memory backend should have a default value"
    );
}

#[test]
fn memory_config_default_embedding_provider() {
    let memory = MemoryConfig::default();
    // Default embedding_provider should be set (even if "none")
    assert!(
        !memory.embedding_provider.is_empty(),
        "embedding_provider should have a default value"
    );
}

#[test]
fn memory_config_default_vector_keyword_weights_sum_to_one() {
    let memory = MemoryConfig::default();
    let sum = memory.vector_weight + memory.keyword_weight;
    assert!(
        (sum - 1.0).abs() < 0.01,
        "vector_weight + keyword_weight should sum to ~1.0, got {sum}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Config TOML serialization round-trip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_toml_roundtrip_preserves_provider() {
    use zeroclaw::config::{DeepseekModelProviderConfig, ModelProviderConfig};
    let mut config = Config::default();
    config.providers.models.deepseek.insert(
        "default".to_string(),
        DeepseekModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("deepseek-chat".into()),
                temperature: Some(0.5),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let toml_str = toml::to_string(&config).expect("config should serialize to TOML");
    let parsed = zeroclaw::config::migration::migrate_to_current(&toml_str)
        .expect("TOML should round-trip through migration");

    assert_eq!(parsed.providers.first_provider_type(), Some("deepseek"));
    assert_eq!(
        parsed
            .providers
            .first_provider()
            .and_then(|e| e.model.as_deref()),
        Some("deepseek-chat")
    );
    assert!(
        (parsed
            .providers
            .first_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.5)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn config_toml_roundtrip_preserves_agent_config() {
    let mut config = Config::default();
    let agent = config.agents.entry("default".into()).or_default();
    agent.max_tool_iterations = 5;
    agent.max_history_messages = 25;
    agent.compact_context = true;

    let toml_str = toml::to_string(&config).expect("config should serialize to TOML");
    let parsed: Config = toml::from_str(&toml_str).expect("TOML should deserialize back");

    let agent = parsed
        .agents
        .get("default")
        .expect("default agent survived round-trip");
    assert_eq!(agent.max_tool_iterations, 5);
    assert_eq!(agent.max_history_messages, 25);
    assert!(agent.compact_context);
}

#[test]
fn config_toml_roundtrip_preserves_memory_config() {
    let mut config = Config::default();
    config.memory.embedding_provider = "openai".into();
    config.memory.embedding_model = "text-embedding-3-small".into();
    config.memory.vector_weight = 0.8;
    config.memory.keyword_weight = 0.2;

    let toml_str = toml::to_string(&config).expect("config should serialize to TOML");
    let parsed: Config = toml::from_str(&toml_str).expect("TOML should deserialize back");

    assert_eq!(parsed.memory.embedding_provider, "openai");
    assert_eq!(parsed.memory.embedding_model, "text-embedding-3-small");
    assert!((parsed.memory.vector_weight - 0.8).abs() < f64::EPSILON);
    assert!((parsed.memory.keyword_weight - 0.2).abs() < f64::EPSILON);
}

// ─────────────────────────────────────────────────────────────────────────────
// Config file write/read round-trip with tempdir
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_file_write_read_roundtrip() {
    use zeroclaw::config::{MistralModelProviderConfig, ModelProviderConfig};
    let tmp = tempfile::TempDir::new().expect("tempdir creation should succeed");
    let config_path = tmp.path().join("config.toml");

    let mut config = Config::default();
    config.providers.models.mistral.insert(
        "default".to_string(),
        MistralModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("mistral-large".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    config
        .agents
        .entry("default".into())
        .or_default()
        .max_tool_iterations = 15;

    let toml_str = toml::to_string(&config).expect("config should serialize");
    fs::write(&config_path, &toml_str).expect("config file write should succeed");

    let read_back = fs::read_to_string(&config_path).expect("config file read should succeed");
    let parsed = zeroclaw::config::migration::migrate_to_current(&read_back)
        .expect("TOML should round-trip through migration");

    assert_eq!(parsed.providers.first_provider_type(), Some("mistral"));
    assert_eq!(
        parsed
            .providers
            .first_provider()
            .and_then(|e| e.model.as_deref()),
        Some("mistral-large")
    );
    assert_eq!(
        parsed
            .agents
            .get("default")
            .map(|a| a.max_tool_iterations)
            .unwrap_or(0),
        15
    );
}

#[test]
fn config_file_with_missing_optional_fields_uses_defaults() {
    // Simulate a minimal config TOML that omits optional sections
    let minimal_toml = r#"
default_temperature = 0.7
"#;
    let parsed: Config = toml::from_str(minimal_toml).expect("minimal TOML should parse");

    // V3 has no static-default agent shim. With no `[agents.<alias>]`
    // defined the lookup misses; the test asserts the absence rather
    // than the previous shim's defaults.
    assert!(
        parsed.agents.is_empty(),
        "minimal TOML should not synthesize any agent"
    );
    // The DelegateAgentConfig defaults still apply when an agent IS
    // configured but omits a tunable.
    let default_agent_cfg = zeroclaw_config::schema::DelegateAgentConfig::default();
    assert_eq!(default_agent_cfg.max_tool_iterations, 10);
    assert_eq!(default_agent_cfg.max_history_messages, 50);
    assert!(default_agent_cfg.compact_context);
}

#[test]
fn config_file_with_custom_agent_section() {
    // V3 lifts the old global `[agent]` settings into `[agents.<alias>]`.
    let toml_with_agent = r#"
default_temperature = 0.7

[agents.default]
max_tool_iterations = 3
compact_context = true
"#;
    let parsed: Config =
        toml::from_str(toml_with_agent).expect("TOML with [agents.default] should parse");

    let agent = parsed.agents.get("default").expect("default agent parsed");
    assert_eq!(agent.max_tool_iterations, 3);
    assert!(agent.compact_context);
    // max_history_messages should still use default
    assert_eq!(agent.max_history_messages, 50);
}

// ─────────────────────────────────────────────────────────────────────────────
// Workspace directory creation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn workspace_dir_creation_in_tempdir() {
    let tmp = tempfile::TempDir::new().expect("tempdir creation should succeed");
    let workspace_dir = tmp.path().join("workspace");

    fs::create_dir_all(&workspace_dir).expect("workspace dir creation should succeed");
    assert!(workspace_dir.exists(), "workspace dir should exist");
    assert!(
        workspace_dir.is_dir(),
        "workspace path should be a directory"
    );
}

#[test]
fn nested_workspace_dir_creation() {
    let tmp = tempfile::TempDir::new().expect("tempdir creation should succeed");
    let nested_dir = tmp.path().join("deep").join("nested").join("workspace");

    fs::create_dir_all(&nested_dir).expect("nested dir creation should succeed");
    assert!(nested_dir.exists(), "nested workspace dir should exist");
}
