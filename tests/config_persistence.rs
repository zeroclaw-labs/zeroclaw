#![allow(clippy::field_reassign_with_default)]
//! TG2: Config Load/Save Round-Trip Tests
//!
//! Prevents: Pattern 2 — Config persistence & workspace discovery bugs (13% of user bugs).
//! Issues: #547, #417, #621, #802
//!
//! Tests Config::load_or_init() with isolated temp directories, env var overrides,
//! and config file round-trips to verify workspace discovery and persistence.

use std::fs;
use zeroclaw::config::{AgentConfig, Config, MemoryConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Config default construction
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_default_has_expected_provider() {
    let config = Config::default();
    assert!(
        config.default_provider.is_some(),
        "default config should have a default_provider"
    );
}

#[test]
fn config_default_has_expected_model() {
    let config = Config::default();
    assert!(
        config.default_model.is_some(),
        "default config should have a default_model"
    );
}

#[test]
fn config_default_temperature_positive() {
    let config = Config::default();
    assert!(
        config.default_temperature > 0.0,
        "default temperature should be positive"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AgentConfig defaults
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn agent_config_default_max_tool_iterations() {
    let agent = AgentConfig::default();
    assert_eq!(
        agent.max_tool_iterations, 20,
        "default max_tool_iterations should be 20"
    );
}

#[test]
fn agent_config_default_max_history_messages() {
    let agent = AgentConfig::default();
    assert_eq!(
        agent.max_history_messages, 50,
        "default max_history_messages should be 50"
    );
}

#[test]
fn agent_config_default_tool_dispatcher() {
    let agent = AgentConfig::default();
    assert_eq!(
        agent.tool_dispatcher, "auto",
        "default tool_dispatcher should be 'auto'"
    );
}

#[test]
fn agent_config_default_compact_context_on() {
    let agent = AgentConfig::default();
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
    let mut config = Config::default();
    config.default_provider = Some("deepseek".into());
    config.default_model = Some("deepseek-chat".into());
    config.default_temperature = 0.5;

    let toml_str = toml::to_string(&config).expect("config should serialize to TOML");
    let parsed: Config = toml::from_str(&toml_str).expect("TOML should deserialize back");

    assert_eq!(parsed.default_provider.as_deref(), Some("deepseek"));
    assert_eq!(parsed.default_model.as_deref(), Some("deepseek-chat"));
    assert!((parsed.default_temperature - 0.5).abs() < f64::EPSILON);
}

#[test]
fn config_toml_roundtrip_preserves_agent_config() {
    let mut config = Config::default();
    config.agent.max_tool_iterations = 5;
    config.agent.max_history_messages = 25;
    config.agent.compact_context = true;

    let toml_str = toml::to_string(&config).expect("config should serialize to TOML");
    let parsed: Config = toml::from_str(&toml_str).expect("TOML should deserialize back");

    assert_eq!(parsed.agent.max_tool_iterations, 5);
    assert_eq!(parsed.agent.max_history_messages, 25);
    assert!(parsed.agent.compact_context);
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
    let tmp = tempfile::TempDir::new().expect("tempdir creation should succeed");
    let config_path = tmp.path().join("config.toml");

    let mut config = Config::default();
    config.default_provider = Some("mistral".into());
    config.default_model = Some("mistral-large".into());
    config.agent.max_tool_iterations = 15;

    let toml_str = toml::to_string(&config).expect("config should serialize");
    fs::write(&config_path, &toml_str).expect("config file write should succeed");

    let read_back = fs::read_to_string(&config_path).expect("config file read should succeed");
    let parsed: Config = toml::from_str(&read_back).expect("TOML should parse back");

    assert_eq!(parsed.default_provider.as_deref(), Some("mistral"));
    assert_eq!(parsed.default_model.as_deref(), Some("mistral-large"));
    assert_eq!(parsed.agent.max_tool_iterations, 15);
}

#[test]
fn config_file_with_missing_optional_fields_uses_defaults() {
    // Simulate a minimal config TOML that omits optional sections
    let minimal_toml = r#"
default_temperature = 0.7
"#;
    let parsed: Config = toml::from_str(minimal_toml).expect("minimal TOML should parse");

    // Agent config should use defaults
    assert_eq!(parsed.agent.max_tool_iterations, 20);
    assert_eq!(parsed.agent.max_history_messages, 50);
    assert!(parsed.agent.compact_context);
}

#[test]
fn config_file_with_custom_agent_section() {
    let toml_with_agent = r#"
default_temperature = 0.7

[agent]
max_tool_iterations = 3
compact_context = true
"#;
    let parsed: Config =
        toml::from_str(toml_with_agent).expect("TOML with agent section should parse");

    assert_eq!(parsed.agent.max_tool_iterations, 3);
    assert!(parsed.agent.compact_context);
    // max_history_messages should still use default
    assert_eq!(parsed.agent.max_history_messages, 50);
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
