use async_trait::async_trait;
use sha2::{Digest, Sha256};

use zeroclaw_api::embedding::EmbeddingProvider;
use zeroclaw_config::provider_registry::ResolvedEmbedding;

use crate::transport::HttpTransportConfig;

const EMBEDDING_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

// ── Noop model_provider (keyword-only fallback) ────────────────────

pub struct NoopEmbedding;

#[async_trait]
impl EmbeddingProvider for NoopEmbedding {
    fn name(&self) -> &str {
        "none"
    }

    fn dimensions(&self) -> usize {
        0
    }

    async fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(Vec::new())
    }
}

// ── OpenAI-compatible embedding model_provider ─────────────────────

pub struct OpenAiEmbedding {
    transport: HttpTransportConfig,
    provider_name: String,
    model: String,
    dims: usize,
}

impl OpenAiEmbedding {
    fn from_config(config: EmbeddingProviderConfig) -> Self {
        Self {
            transport: config.transport,
            provider_name: config.provider_name,
            model: config.model,
            dims: config.dimensions,
        }
    }

    fn embeddings_url(&self) -> anyhow::Result<String> {
        self.transport.endpoint("embeddings")
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedding {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn identity(&self) -> zeroclaw_api::embedding::EmbeddingIdentity {
        let mut digest = Sha256::new();
        digest.update(self.transport.base_url.trim_end_matches('/').as_bytes());
        for (name, value) in &self.transport.extra_headers {
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
        zeroclaw_api::embedding::EmbeddingIdentity {
            // Persist the wire protocol namespace, independently of the
            // configured connection kind used for display/attribution. A
            // registry promotion must not change an unchanged vector space.
            provider: format!("openai:{:x}", digest.finalize()),
            model: self.model.clone(),
            dimensions: self.dims,
        }
    }

    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let body = crate::protocols::openai::embedding::EmbeddingRequest {
            model: &self.model,
            input: texts,
        };

        let client = self
            .transport
            .client(EMBEDDING_HTTP_TIMEOUT, "memory.embeddings")?;
        let resp = self
            .transport
            .bearer(client.post(self.embeddings_url()?))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        crate::protocols::openai::embedding::vectors(resp, texts.len(), self.dims).await
    }
}

/// Fully resolved construction input for the shared OpenAI embeddings wire.
#[derive(Clone)]
pub struct EmbeddingProviderConfig {
    pub(crate) transport: HttpTransportConfig,
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) dimensions: usize,
}

impl EmbeddingProviderConfig {
    pub fn from_resolved(
        resolved: &ResolvedEmbedding<'_>,
        fallback_dimensions: usize,
    ) -> anyhow::Result<Self> {
        let model = resolved.model.trim();
        if model.is_empty() {
            anyhow::bail!("embedding model name must not be empty");
        }
        let dimensions = resolved.dimensions.unwrap_or(fallback_dimensions);
        if dimensions == 0 {
            anyhow::bail!("embedding dimensions must be greater than zero");
        }
        Ok(Self {
            transport: HttpTransportConfig::from_connection(resolved.selection.connection, None)?,
            provider_name: resolved.selection.connection.kind().to_owned(),
            model: model.to_owned(),
            dimensions,
        })
    }
}

pub fn build_embedding_provider(config: EmbeddingProviderConfig) -> Box<dyn EmbeddingProvider> {
    Box::new(OpenAiEmbedding::from_config(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved_config(
        kind: &str,
        uri: Option<&str>,
        key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) -> EmbeddingProviderConfig {
        let mut connection = serde_json::json!({"kind": kind});
        if let Some(uri) = uri {
            connection["uri"] = uri.into();
        }
        if let Some(key) = key {
            connection["auth"] = serde_json::json!({"mode": "api_key", "api_key": key});
        }
        let registry: zeroclaw_config::provider_registry::ProviderRegistry = serde_json::from_value(serde_json::json!({
            "connections": {"fixture": connection},
            "models": {"embedding.fixture": {"capability": "embedding", "provider": "fixture", "model": model, "metadata": {"dimensions": dimensions}}}
        })).unwrap();
        EmbeddingProviderConfig::from_resolved(
            &registry
                .resolve_embedding(&"embedding.fixture".into())
                .unwrap(),
            dimensions,
        )
        .unwrap()
    }

    #[test]
    fn resolved_embedding_owns_credentials_and_dimensions() {
        use zeroclaw_config::provider_registry::ProviderRegistry;

        for connection_key in [Some("embedding-key"), None] {
            let mut connection = serde_json::json!({
                "kind": "custom",
                "uri": "https://embedding.example.test/v1",
            });
            if let Some(key) = connection_key {
                connection["auth"] = serde_json::json!({"mode": "api_key", "api_key": key});
            }
            let registry: ProviderRegistry = serde_json::from_value(serde_json::json!({
                "connections": {"embedding.prod": connection},
                "models": {"embedding.search.v2": {
                    "capability": "embedding",
                    "provider": "embedding.prod",
                    "model": "embedding-model",
                    "metadata": {"dimensions": 3072},
                }},
            }))
            .unwrap();
            let resolved = registry
                .resolve_embedding(&"embedding.search.v2".into())
                .unwrap();
            let config = EmbeddingProviderConfig::from_resolved(&resolved, 1536).unwrap();
            assert_eq!(config.dimensions, 3072);
            assert_eq!(config.model, "embedding-model");
            assert_eq!(config.transport.api_key.as_deref(), connection_key);
        }
    }

    #[test]
    fn noop_name() {
        let p = NoopEmbedding;
        assert_eq!(p.name(), "none");
        assert_eq!(p.dimensions(), 0);
    }

    #[tokio::test]
    async fn noop_embed_returns_empty() {
        let p = NoopEmbedding;
        let result = p.embed(&["hello"]).await.unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn factory_openai() {
        let p = build_embedding_provider(resolved_config(
            "openai",
            None,
            Some("key"),
            "text-embedding-3-small",
            1536,
        ));
        assert_eq!(p.name(), "openai");
        assert_eq!(p.dimensions(), 1536);
    }

    #[test]
    fn factory_openrouter() {
        let p = build_embedding_provider(resolved_config(
            "openrouter",
            None,
            Some("sk-or-test"),
            "openai/text-embedding-3-small",
            1536,
        ));
        assert_eq!(p.name(), "openrouter");
        assert_eq!(p.dimensions(), 1536);
    }

    #[test]
    fn factory_custom_url() {
        let p = build_embedding_provider(resolved_config(
            "custom",
            Some("http://localhost:1234"),
            None,
            "model",
            768,
        ));
        assert_eq!(p.name(), "custom");
        assert_eq!(p.dimensions(), 768);
    }

    #[test]
    fn compatible_identity_tracks_vector_space_independently_of_connection_kind() {
        let baseline = OpenAiEmbedding::from_config(resolved_config(
            "openai",
            Some("https://embedding.example.test/v1/"),
            Some("old-key"),
            "model",
            4,
        ));
        for kind in ["openai", "custom", "vllm", "openrouter"] {
            let mut provider = OpenAiEmbedding::from_config(EmbeddingProviderConfig {
                transport: HttpTransportConfig::with_base_url(
                    "https://embedding.example.test/v1/",
                    Some("new-key".into()),
                ),
                provider_name: kind.into(),
                model: "model".into(),
                dimensions: 4,
            });
            assert_eq!(provider.name(), kind);
            assert_eq!(provider.identity(), baseline.identity());
            provider
                .transport
                .extra_headers
                .insert("X-Tenant".into(), "other".into());
            assert_ne!(provider.identity(), baseline.identity());
            provider.transport.extra_headers.clear();
            provider.transport.base_url = "https://other.example.test/v1".into();
            assert_ne!(provider.identity(), baseline.identity());
            provider.transport.base_url = baseline.transport.base_url.clone();
            provider.model = "other-model".into();
            assert_ne!(provider.identity(), baseline.identity());
            provider.model = "model".into();
            provider.dims = 8;
            assert_ne!(provider.identity(), baseline.identity());
        }
    }

    // ── Edge cases ───────────────────────────────────────────────

    #[tokio::test]
    async fn noop_embed_one_returns_error() {
        let p = NoopEmbedding;
        // embed returns empty vec → pop() returns None → error
        let result = p.embed_one("hello").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn noop_embed_empty_batch() {
        let p = NoopEmbedding;
        let result = p.embed(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn noop_embed_multiple_texts() {
        let p = NoopEmbedding;
        let result = p.embed(&["a", "b", "c"]).await.unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn factory_openai_no_api_key() {
        let p = build_embedding_provider(resolved_config(
            "openai",
            None,
            None,
            "text-embedding-3-small",
            1536,
        ));
        assert_eq!(p.name(), "openai");
        assert_eq!(p.dimensions(), 1536);
    }

    #[test]
    fn openai_trailing_slash_does_not_duplicate_endpoint_path() {
        let p = OpenAiEmbedding::from_config(resolved_config(
            "openai",
            Some("https://api.openai.com/"),
            Some("key"),
            "model",
            1536,
        ));
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://api.openai.com/v1/embeddings"
        );
    }

    #[test]
    fn openai_dimensions_custom() {
        let p = OpenAiEmbedding::from_config(resolved_config(
            "openai",
            Some("http://localhost"),
            Some("k"),
            "m",
            384,
        ));
        assert_eq!(p.dimensions(), 384);
    }

    #[test]
    fn embeddings_url_openrouter() {
        let p = OpenAiEmbedding::from_config(resolved_config(
            "openai",
            Some("https://openrouter.ai/api/v1"),
            Some("key"),
            "openai/text-embedding-3-small",
            1536,
        ));
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://openrouter.ai/api/v1/embeddings"
        );
    }

    #[test]
    fn embeddings_url_standard_openai() {
        let p = OpenAiEmbedding::from_config(resolved_config(
            "openai",
            Some("https://api.openai.com"),
            Some("key"),
            "model",
            1536,
        ));
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://api.openai.com/v1/embeddings"
        );
    }

    #[test]
    fn embeddings_url_base_with_v1_no_duplicate() {
        let p = OpenAiEmbedding::from_config(resolved_config(
            "openai",
            Some("https://api.example.com/v1"),
            Some("key"),
            "model",
            1536,
        ));
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://api.example.com/v1/embeddings"
        );
    }

    #[test]
    fn embeddings_url_non_v1_api_path_uses_raw_suffix() {
        let p = OpenAiEmbedding::from_config(resolved_config(
            "openai",
            Some("https://api.example.com/api/coding/v3"),
            Some("key"),
            "model",
            1536,
        ));
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://api.example.com/api/coding/v3/embeddings"
        );
    }

    #[test]
    fn embeddings_url_custom_full_endpoint() {
        let p = OpenAiEmbedding::from_config(resolved_config(
            "openai",
            Some("https://my-api.example.com/api/v2/embeddings"),
            Some("key"),
            "model",
            1536,
        ));
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://my-api.example.com/api/v2/embeddings"
        );
    }
}
