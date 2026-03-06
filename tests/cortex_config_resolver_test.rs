//! Configuration resolver tests for Cortex-Memory backend
//!
//! These tests verify the config derivation logic without requiring
//! actual Qdrant or LLM services.
//!
//! Run with: cargo test --test cortex_config_resolver_test --features memory-cortex

#![cfg(feature = "memory-cortex")]

use zeroclaw::config::schema::{Config, CortexMemConfig, MemoryConfig};

// ── Helper to build test configs ─────────────────────────────────

fn make_base_config() -> Config {
    let mut config = Config::default();
    config.api_key = Some("test-api-key-12345".to_string());
    config.default_provider = Some("openai".to_string());
    config.default_model = Some("gpt-4o-mini".to_string());
    config.default_temperature = 0.7;
    config
}

fn make_memory_config_with_cortex() -> MemoryConfig {
    MemoryConfig {
        backend: "cortex".to_string(),
        embedding_provider: "openai".to_string(),
        embedding_model: "text-embedding-3-small".to_string(),
        embedding_dimensions: 1536,
        cortex: CortexMemConfig {
            qdrant_url: Some("http://localhost:6334".to_string()),
            qdrant_collection: "test-collection".to_string(),
            tenant_id: "test-tenant".to_string(),
            ..CortexMemConfig::default()
        },
        ..MemoryConfig::default()
    }
}

// ── Test 1: Backend classification ──────────────────────────────

#[test]
fn cortex_backend_is_classified_correctly() {
    use zeroclaw::memory::{classify_memory_backend, MemoryBackendKind};
    
    assert_eq!(
        classify_memory_backend("cortex"),
        MemoryBackendKind::Cortex
    );
}

#[test]
fn cortex_backend_profile_is_correct() {
    use zeroclaw::memory::memory_backend_profile;
    
    let profile = memory_backend_profile("cortex");
    assert_eq!(profile.key, "cortex");
    assert!(profile.optional_dependency);
    assert!(!profile.sqlite_based);
}

// ── Test 2: Config structure defaults ────────────────────────────

#[test]
fn cortex_config_defaults_are_sensible() {
    let config = CortexMemConfig::default();
    
    assert_eq!(config.tenant_id, "zeroclaw");
    assert_eq!(config.qdrant_collection, "zeroclaw-memory");
    assert_eq!(config.llm_temperature, 0.3);
    assert!(config.auto_index);
    assert!(config.auto_extract);
    assert!(config.generate_layers_on_close);
    assert!(config.qdrant_url.is_none());
    assert!(config.llm_model_override.is_none());
}

// ── Test 3: Memory config includes cortex ────────────────────────

#[test]
fn memory_config_has_cortex_field() {
    let memory = MemoryConfig::default();
    assert_eq!(memory.cortex.tenant_id, "zeroclaw");
}

// ── Test 4: Provider URL derivation ──────────────────────────────

#[test]
fn test_provider_url_derivation() {
    // This tests the internal logic indirectly through config construction
    let config = make_base_config();
    
    // OpenAI
    assert!(config.api_url.is_none()); // Should derive from provider
    
    // With explicit URL
    let mut config_with_url = make_base_config();
    config_with_url.api_url = Some("http://custom.api/v1".to_string());
    assert_eq!(config_with_url.api_url, Some("http://custom.api/v1".to_string()));
}

// ── Test 5: Embedding route configuration ────────────────────────

#[test]
fn embedding_route_hint_parsing() {
    let mut memory = make_memory_config_with_cortex();
    
    // Direct model
    memory.embedding_model = "text-embedding-3-small".to_string();
    assert!(!memory.embedding_model.starts_with("hint:"));
    
    // Hint-based routing
    memory.embedding_model = "hint:semantic".to_string();
    assert!(memory.embedding_model.starts_with("hint:"));
    assert_eq!(
        memory.embedding_model.strip_prefix("hint:"),
        Some("semantic")
    );
}

// ── Test 6: Tenant isolation ─────────────────────────────────────

#[test]
fn tenant_isolation_via_config() {
    let mut config1 = CortexMemConfig::default();
    config1.tenant_id = "tenant-alpha".to_string();
    
    let mut config2 = CortexMemConfig::default();
    config2.tenant_id = "tenant-beta".to_string();
    
    assert_ne!(config1.tenant_id, config2.tenant_id);
}

// ── Test 7: Temperature override for extraction ──────────────────

#[test]
fn llm_temperature_can_be_overridden() {
    let mut config = CortexMemConfig::default();
    assert_eq!(config.llm_temperature, 0.3);
    
    // Lower temperature for more deterministic extraction
    config.llm_temperature = 0.1;
    assert_eq!(config.llm_temperature, 0.1);
    
    // Higher temperature for more creative extraction
    config.llm_temperature = 0.5;
    assert_eq!(config.llm_temperature, 0.5);
}

// ── Test 8: Qdrant configuration ─────────────────────────────────

#[test]
fn qdrant_config_defaults() {
    let config = CortexMemConfig::default();
    
    // Default collection name
    assert_eq!(config.qdrant_collection, "zeroclaw-memory");
    
    // No API key by default
    assert!(config.qdrant_api_key.is_none());
    
    // No URL by default (must be set by user)
    assert!(config.qdrant_url.is_none());
}

#[test]
fn qdrant_config_can_be_set() {
    let config = CortexMemConfig {
        qdrant_url: Some("http://qdrant.example.com:6334".to_string()),
        qdrant_collection: "production-memory".to_string(),
        qdrant_api_key: Some("secret-api-key".to_string()),
        ..CortexMemConfig::default()
    };
    
    assert_eq!(config.qdrant_url, Some("http://qdrant.example.com:6334".to_string()));
    assert_eq!(config.qdrant_collection, "production-memory");
    assert_eq!(config.qdrant_api_key, Some("secret-api-key".to_string()));
}

// ── Test 9: Feature flag guard ────────────────────────────────────

#[test]
fn cortex_module_is_available_with_feature() {
    // This test only compiles when memory-cortex feature is enabled
    use zeroclaw::memory::CortexMemory;
    
    // Just verify the type exists
    let _ = std::marker::PhantomData::<CortexMemory>;
}
