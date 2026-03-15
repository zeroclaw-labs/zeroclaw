//! Cortex-Memory configuration resolver
//!
//! Auto-derives cortex-mem configuration from zeroclaw's global settings,
//! providing zero-configuration integration for users.

use crate::config::schema::CortexMemConfig;
use crate::config::{Config, MemoryConfig};
use anyhow::{Context, Result};

/// Resolved cortex-mem configuration
pub struct ResolvedCortexConfig {
    pub data_dir: String,
    pub tenant_id: String,
    pub qdrant_url: String,
    pub qdrant_collection: String,
    pub qdrant_api_key: Option<String>,
    pub llm_api_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_temperature: f32,
    pub embedding_api_base_url: String,
    pub embedding_api_key: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub auto_extract: bool,
    pub generate_layers_on_close: bool,
}

/// Resolve cortex-mem configuration from zeroclaw config
///
/// This function automatically derives cortex-mem's LLM and Embedding
/// configuration from zeroclaw's global settings, minimizing user configuration burden.
pub fn resolve_cortex_config(
    zeroclaw_config: &Config,
    memory_config: &MemoryConfig,
    workspace_dir: &std::path::Path,
) -> Result<ResolvedCortexConfig> {
    let cortex_config = &memory_config.cortex;

    // 1. Resolve data directory
    let data_dir = cortex_config.data_dir.clone().unwrap_or_else(|| {
        workspace_dir
            .join("cortex-data")
            .to_string_lossy()
            .to_string()
    });

    // 2. Resolve tenant ID
    let tenant_id = cortex_config.tenant_id.clone();

    // 3. Resolve Qdrant configuration
    let qdrant_url = cortex_config
        .qdrant_url
        .clone()
        .unwrap_or_else(|| "http://localhost:6334".to_string());
    let qdrant_collection = cortex_config.qdrant_collection.clone();
    let qdrant_api_key = cortex_config.qdrant_api_key.clone();

    // 4. Resolve LLM configuration (auto-derive from zeroclaw settings)
    let (llm_api_base_url, llm_api_key, llm_model) =
        resolve_llm_config(zeroclaw_config, cortex_config)?;
    let llm_temperature = cortex_config.llm_temperature;

    // 5. Resolve Embedding configuration (support hint: routing)
    let (embedding_api_base_url, embedding_api_key, embedding_model, embedding_dimensions) =
        resolve_embedding_config(zeroclaw_config, memory_config, cortex_config)?;

    Ok(ResolvedCortexConfig {
        data_dir,
        tenant_id,
        qdrant_url,
        qdrant_collection,
        qdrant_api_key,
        llm_api_base_url,
        llm_api_key,
        llm_model,
        llm_temperature,
        embedding_api_base_url,
        embedding_api_key,
        embedding_model,
        embedding_dimensions,
        auto_extract: cortex_config.auto_extract,
        generate_layers_on_close: cortex_config.generate_layers_on_close,
    })
}

/// Resolve LLM configuration from zeroclaw settings
///
/// Priority:
/// 1. cortex_config.llm_model_override
/// 2. zeroclaw_config.default_provider + default_model
fn resolve_llm_config(
    zeroclaw_config: &Config,
    cortex_config: &CortexMemConfig,
) -> Result<(String, String, String)> {
    tracing::debug!(
        "Resolving LLM config: api_url={:?}, default_provider={:?}, default_model={:?}",
        zeroclaw_config.api_url,
        zeroclaw_config.default_provider,
        zeroclaw_config.default_model
    );

    // Get API key (required)
    let llm_api_key = zeroclaw_config
        .api_key
        .clone()
        .context("Cortex-Memory requires api_key in zeroclaw config")?;

    // Get API base URL
    let llm_api_base_url = zeroclaw_config.api_url.clone().unwrap_or_else(|| {
        let derived = derive_provider_base_url(zeroclaw_config.default_provider.as_deref());
        tracing::debug!(
            "LLM API base URL derived from provider {:?}: {}",
            zeroclaw_config.default_provider,
            derived
        );
        derived
    });

    // Get model
    let llm_model = cortex_config
        .llm_model_override
        .clone()
        .or_else(|| zeroclaw_config.default_model.clone())
        .unwrap_or_else(|| "gpt-3.5-turbo".to_string());

    Ok((llm_api_base_url, llm_api_key, llm_model))
}

/// Resolve Embedding configuration from zeroclaw settings
///
/// Supports:
/// - hint: routing via embedding_routes
/// - custom:URL provider
/// - Direct configuration
fn resolve_embedding_config(
    zeroclaw_config: &Config,
    memory_config: &MemoryConfig,
    cortex_config: &CortexMemConfig,
) -> Result<(String, String, String, usize)> {
    tracing::debug!(
        "Resolving embedding config: provider={}, model={}",
        memory_config.embedding_provider,
        memory_config.embedding_model
    );

    // Check if using hint routing
    if let Some(hint) = memory_config.embedding_model.strip_prefix("hint:") {
        return resolve_embedding_from_route(zeroclaw_config, hint, cortex_config);
    }

    // Direct configuration
    let embedding_api_base_url = provider_to_base_url(&memory_config.embedding_provider);
    let embedding_model = memory_config.embedding_model.clone();
    let embedding_dimensions = memory_config.embedding_dimensions;

    tracing::debug!(
        "Embedding API base URL resolved to: {}",
        embedding_api_base_url
    );

    // API key: cortex override > global api_key
    let embedding_api_key = cortex_config
        .embedding_api_key_override
        .clone()
        .or_else(|| zeroclaw_config.api_key.clone())
        .context(
            "Embedding requires api_key (set globally or via cortex.embedding_api_key_override)",
        )?;

    Ok((
        embedding_api_base_url,
        embedding_api_key,
        embedding_model,
        embedding_dimensions,
    ))
}

/// Resolve embedding config from embedding_routes
fn resolve_embedding_from_route(
    zeroclaw_config: &Config,
    hint: &str,
    cortex_config: &CortexMemConfig,
) -> Result<(String, String, String, usize)> {
    let route = zeroclaw_config
        .embedding_routes
        .iter()
        .find(|r| r.hint == hint)
        .with_context(|| format!("No matching embedding_route for hint: {}", hint))?;

    let embedding_api_base_url = provider_to_base_url(&route.provider);
    let embedding_model = route.model.clone();
    let embedding_dimensions = route.dimensions.unwrap_or(1536);

    // API key: route override > cortex override > global
    let embedding_api_key = route
        .api_key
        .clone()
        .or_else(|| cortex_config.embedding_api_key_override.clone())
        .or_else(|| zeroclaw_config.api_key.clone())
        .context("Embedding route requires api_key")?;

    Ok((
        embedding_api_base_url,
        embedding_api_key,
        embedding_model,
        embedding_dimensions,
    ))
}

/// Convert provider name to base URL
///
/// Supports:
/// - custom:URL format (e.g., "custom:http://localhost:11434/v1")
/// - Known providers: openai, anthropic, ollama, openrouter
/// - Falls back to OpenAI for unknown providers (documented behavior)
fn provider_to_base_url(provider: &str) -> String {
    if let Some(custom_url) = provider.strip_prefix("custom:") {
        return custom_url.to_string();
    }

    match provider {
        "openai" => "https://api.openai.com/v1".to_string(),
        "anthropic" => "https://api.anthropic.com/v1".to_string(),
        "ollama" => "http://localhost:11434/v1".to_string(),
        "openrouter" => "https://openrouter.ai/api/v1".to_string(),
        // Fallback to OpenAI for unknown providers
        // This is intentional for backward compatibility
        _ => "https://api.openai.com/v1".to_string(),
    }
}

/// Derive base URL from provider name
fn derive_provider_base_url(provider: Option<&str>) -> String {
    match provider {
        Some("openai") => "https://api.openai.com/v1".to_string(),
        Some("anthropic") => "https://api.anthropic.com/v1".to_string(),
        Some("ollama") => "http://localhost:11434/v1".to_string(),
        Some("openrouter") => "https://openrouter.ai/api/v1".to_string(),
        Some(custom) if custom.starts_with("custom:") => {
            custom.strip_prefix("custom:").unwrap().to_string()
        }
        _ => "https://api.openai.com/v1".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::CortexMemConfig;

    fn make_test_config() -> Config {
        let mut config = Config::default();
        config.api_key = Some("test-api-key".to_string());
        config.default_provider = Some("openai".to_string());
        config.default_model = Some("gpt-4o-mini".to_string());
        config.default_temperature = 0.7;
        config
    }

    #[test]
    fn test_resolve_llm_config_from_global() {
        let zeroclaw_config = make_test_config();
        let cortex_config = CortexMemConfig::default();

        let (base_url, api_key, model) =
            resolve_llm_config(&zeroclaw_config, &cortex_config).unwrap();

        assert_eq!(base_url, "https://api.openai.com/v1");
        assert_eq!(api_key, "test-api-key");
        assert_eq!(model, "gpt-4o-mini");
    }

    #[test]
    fn test_resolve_llm_config_with_override() {
        let zeroclaw_config = make_test_config();
        let cortex_config = CortexMemConfig {
            llm_model_override: Some("gpt-4".to_string()),
            llm_temperature: 0.3,
            ..CortexMemConfig::default()
        };

        let (_, _, model) = resolve_llm_config(&zeroclaw_config, &cortex_config).unwrap();

        assert_eq!(model, "gpt-4");
    }

    #[test]
    fn test_provider_to_base_url() {
        assert_eq!(provider_to_base_url("openai"), "https://api.openai.com/v1");
        assert_eq!(
            provider_to_base_url("anthropic"),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(provider_to_base_url("ollama"), "http://localhost:11434/v1");
        assert_eq!(
            provider_to_base_url("openrouter"),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            provider_to_base_url("custom:http://localhost:8080/v1"),
            "http://localhost:8080/v1"
        );
    }

    #[test]
    fn test_derive_provider_base_url() {
        assert_eq!(
            derive_provider_base_url(Some("openai")),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            derive_provider_base_url(Some("ollama")),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            derive_provider_base_url(Some("custom:http://custom.api/v1")),
            "http://custom.api/v1"
        );
    }
}
