//! TG2: Config Load/Save Round-Trip Tests

use std::fs;
use zeroclaw::config::{Config, MemoryConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Config default construction
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_default_has_no_model_provider_profiles() {
    let config = Config::default();
    assert!(
        config.providers.models.is_empty(),
        "default config should not synthesize provider profiles"
    );
    assert_eq!(
        config.providers.models.iter_entries().count(),
        0,
        "default config should have no typed provider entries"
    );
}

#[test]
fn config_default_has_no_resolved_model() {
    let config = Config::default();
    assert_eq!(
        config.resolve_default_model(),
        None,
        "default config should not resolve a model until one is configured"
    );
}

#[test]
fn config_default_validates_without_provider_profiles() {
    let config = Config::default();
    config
        .validate()
        .expect("default config should validate without provider profiles");
}

// ─────────────────────────────────────────────────────────────────────────────
// AliasedAgentConfig defaults
// ─────────────────────────────────────────────────────────────────────────────

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
        },
    );

    let toml_str = toml::to_string(&config).expect("config should serialize to TOML");
    let parsed = zeroclaw::config::migration::migrate_to_current(&toml_str)
        .expect("TOML should round-trip through migration");

    assert!(
        parsed
            .providers
            .models
            .find("deepseek", "default")
            .is_some(),
        "deepseek.default entry should survive round-trip"
    );
    assert_eq!(
        parsed
            .providers
            .models
            .find("deepseek", "default")
            .and_then(|e| e.model.as_deref()),
        Some("deepseek-chat")
    );
    assert!(
        (parsed
            .providers
            .models
            .find("deepseek", "default")
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
    agent.risk_profile = "tight".into();
    agent.runtime_profile = "fast".into();
    agent.enabled = false;

    let toml_str = toml::to_string(&config).expect("config should serialize to TOML");
    let parsed: Config = toml::from_str(&toml_str).expect("TOML should deserialize back");

    let agent = parsed
        .agents
        .get("default")
        .expect("default agent survived round-trip");
    assert_eq!(agent.risk_profile, "tight");
    assert_eq!(agent.runtime_profile, "fast");
    assert!(!agent.enabled);
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
        },
    );
    config
        .agents
        .entry("default".into())
        .or_default()
        .risk_profile = "tight".into();

    let toml_str = toml::to_string(&config).expect("config should serialize");
    fs::write(&config_path, &toml_str).expect("config file write should succeed");

    let read_back = fs::read_to_string(&config_path).expect("config file read should succeed");
    let parsed = zeroclaw::config::migration::migrate_to_current(&read_back)
        .expect("TOML should round-trip through migration");

    assert!(
        parsed.providers.models.find("mistral", "default").is_some(),
        "mistral.default entry should survive round-trip"
    );
    assert_eq!(
        parsed
            .providers
            .models
            .find("mistral", "default")
            .and_then(|e| e.model.as_deref()),
        Some("mistral-large")
    );
    assert_eq!(
        parsed
            .agents
            .get("default")
            .map(|a| a.risk_profile.as_str())
            .unwrap_or(""),
        "tight"
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
}

#[test]
fn config_file_with_custom_agent_section() {
    // V3 lifts the old global `[agent]` settings into `[agents.<alias>]`.
    let toml_with_agent = r#"
default_temperature = 0.7

[agents.default]
risk_profile = "tight"
enabled = true
"#;
    let parsed: Config =
        toml::from_str(toml_with_agent).expect("TOML with [agents.default] should parse");

    let agent = parsed.agents.get("default").expect("default agent parsed");
    assert_eq!(agent.risk_profile, "tight");
    assert!(agent.enabled);
    // runtime_profile is omitted, so it stays the empty default.
    assert_eq!(agent.runtime_profile, "");
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
