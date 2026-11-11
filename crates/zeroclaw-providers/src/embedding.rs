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
    /// Legacy constructor for the old `[memory]` embedding settings.
    pub fn new(base_url: &str, api_key: &str, model: &str, dims: usize) -> Self {
        Self {
            transport: HttpTransportConfig::with_base_url(
                base_url.trim_end_matches('/'),
                Some(api_key.to_string()),
            ),
            provider_name: "openai".to_owned(),
            model: model.to_string(),
            dims,
        }
    }

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
        api_key_override: Option<String>,
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
            transport: {
                let mut transport =
                    HttpTransportConfig::from_connection(resolved.selection.connection, None)?;
                if api_key_override.is_some() {
                    transport.api_key = api_key_override;
                }
                transport
            },
            provider_name: resolved.selection.connection.kind().to_owned(),
            model: model.to_owned(),
            dimensions,
        })
    }
}

pub fn build_embedding_provider(config: EmbeddingProviderConfig) -> Box<dyn EmbeddingProvider> {
    Box::new(OpenAiEmbedding::from_config(config))
}

// ── Factory ──────────────────────────────────────────────────

pub fn create_embedding_provider(
    model_provider: &str,
    api_key: Option<&str>,
    model: &str,
    dims: usize,
) -> Box<dyn EmbeddingProvider> {
    match model_provider {
        "openai" => {
            let key = api_key.unwrap_or("");
            Box::new(OpenAiEmbedding::new(
                "https://api.openai.com",
                key,
                model,
                dims,
            ))
        }
        "openrouter" => {
            let key = api_key.unwrap_or("");
            Box::new(OpenAiEmbedding::new(
                "https://openrouter.ai/api/v1",
                key,
                model,
                dims,
            ))
        }
        name if name.starts_with("custom:") => {
            let base_url = name.strip_prefix("custom:").unwrap_or("");
            let key = api_key.unwrap_or("");
            Box::new(OpenAiEmbedding::new(base_url, key, model, dims))
        }
        _ => Box::new(NoopEmbedding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn factory_none() {
        let p = create_embedding_provider("none", None, "model", 1536);
        assert_eq!(p.name(), "none");
    }

    #[test]
    fn factory_openai() {
        let p = create_embedding_provider("openai", Some("key"), "text-embedding-3-small", 1536);
        assert_eq!(p.name(), "openai");
        assert_eq!(p.dimensions(), 1536);
    }

    #[test]
    fn factory_openrouter() {
        let p = create_embedding_provider(
            "openrouter",
            Some("sk-or-test"),
            "openai/text-embedding-3-small",
            1536,
        );
        assert_eq!(p.name(), "openai"); // uses OpenAiEmbedding internally
        assert_eq!(p.dimensions(), 1536);
    }

    #[test]
    fn factory_custom_url() {
        let p = create_embedding_provider("custom:http://localhost:1234", None, "model", 768);
        assert_eq!(p.name(), "openai"); // uses OpenAiEmbedding internally
        assert_eq!(p.dimensions(), 768);
    }

    #[test]
    fn compatible_identity_tracks_vector_space_independently_of_connection_kind() {
        let legacy =
            OpenAiEmbedding::new("https://embedding.example.test/v1/", "old-key", "model", 4);
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
            assert_eq!(provider.identity(), legacy.identity());
            provider
                .transport
                .extra_headers
                .insert("X-Tenant".into(), "other".into());
            assert_ne!(provider.identity(), legacy.identity());
            provider.transport.extra_headers.clear();
            provider.transport.base_url = "https://other.example.test/v1".into();
            assert_ne!(provider.identity(), legacy.identity());
            provider.transport.base_url = legacy.transport.base_url.clone();
            provider.model = "other-model".into();
            assert_ne!(provider.identity(), legacy.identity());
            provider.model = "model".into();
            provider.dims = 8;
            assert_ne!(provider.identity(), legacy.identity());
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
    fn factory_empty_string_returns_noop() {
        let p = create_embedding_provider("", None, "model", 1536);
        assert_eq!(p.name(), "none");
    }

    #[test]
    fn factory_unknown_provider_returns_noop() {
        let p = create_embedding_provider("cohere", None, "model", 1536);
        assert_eq!(p.name(), "none");
    }

    #[test]
    fn factory_custom_empty_url() {
        // "custom:" with no URL — should still construct without panic
        let p = create_embedding_provider("custom:", None, "model", 768);
        assert_eq!(p.name(), "openai");
    }

    #[test]
    fn factory_openai_no_api_key() {
        let p = create_embedding_provider("openai", None, "text-embedding-3-small", 1536);
        assert_eq!(p.name(), "openai");
        assert_eq!(p.dimensions(), 1536);
    }

    #[test]
    fn openai_trailing_slash_stripped() {
        let p = OpenAiEmbedding::new("https://api.openai.com/", "key", "model", 1536);
        assert_eq!(p.transport.base_url, "https://api.openai.com");
    }

    #[test]
    fn openai_dimensions_custom() {
        let p = OpenAiEmbedding::new("http://localhost", "k", "m", 384);
        assert_eq!(p.dimensions(), 384);
    }

    #[test]
    fn embeddings_url_openrouter() {
        let p = OpenAiEmbedding::new(
            "https://openrouter.ai/api/v1",
            "key",
            "openai/text-embedding-3-small",
            1536,
        );
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://openrouter.ai/api/v1/embeddings"
        );
    }

    #[test]
    fn embeddings_url_standard_openai() {
        let p = OpenAiEmbedding::new("https://api.openai.com", "key", "model", 1536);
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://api.openai.com/v1/embeddings"
        );
    }

    #[test]
    fn embeddings_url_base_with_v1_no_duplicate() {
        let p = OpenAiEmbedding::new("https://api.example.com/v1", "key", "model", 1536);
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://api.example.com/v1/embeddings"
        );
    }

    #[test]
    fn embeddings_url_non_v1_api_path_uses_raw_suffix() {
        let p = OpenAiEmbedding::new(
            "https://api.example.com/api/coding/v3",
            "key",
            "model",
            1536,
        );
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://api.example.com/api/coding/v3/embeddings"
        );
    }

    #[test]
    fn embeddings_url_custom_full_endpoint() {
        let p = OpenAiEmbedding::new(
            "https://my-api.example.com/api/v2/embeddings",
            "key",
            "model",
            1536,
        );
        assert_eq!(
            p.embeddings_url().unwrap(),
            "https://my-api.example.com/api/v2/embeddings"
        );
    }
}
