//! Embedding provider abstraction (PR #1 — Local-first EmbeddingProvider).
//!
//! The trait decouples vector generation from its concrete backend. Production
//! targets:
//!
//! * `local_fastembed` — BGE-M3 via `fastembed` (ONNX, CPU) → primary default
//!   under the `embedding-local` cargo feature. Keeps embeddings on-device —
//!   this is load-bearing for the server-non-storage E2E patent claim.
//! * `openai` — OpenAI-compatible HTTPS endpoint (OpenAI cloud, OpenRouter).
//! * `custom_http` — self-hosted OpenAI-compatible endpoint (TEI, llama.cpp).
//! * `none` — keyword-only fallback (returns empty vectors).
//!
//! Metadata accessors (`provider`, `model`, `version`) feed the PR #2 embedding
//! metadata columns (`vault_documents.embedding_{provider,model,version}`) so
//! we can detect model drift and trigger point re-embedding.

use async_trait::async_trait;

pub mod custom_http;
pub mod local_fastembed;
pub mod noop;
pub mod openai;

pub use local_fastembed::LocalFastembedProvider;
pub use noop::NoopEmbedding;
pub use openai::OpenAiEmbedding;

/// Identifier stored in `vault_documents.embedding_provider` when the embedder
/// serves requests against the local BGE-M3 model.
pub const PROVIDER_LOCAL_FASTEMBED: &str = "local_fastembed";

/// Identifier stored in `vault_documents.embedding_provider` for direct
/// OpenAI endpoints.
pub const PROVIDER_OPENAI: &str = "openai";

/// Identifier stored in `vault_documents.embedding_provider` for a
/// user-supplied OpenAI-compatible endpoint (custom:URL form).
pub const PROVIDER_CUSTOM_HTTP: &str = "custom_http";

/// Identifier stored in `vault_documents.embedding_provider` for the
/// keyword-only fallback.
pub const PROVIDER_NONE: &str = "none";

/// Bumped whenever the embedding semantics change in a way that invalidates
/// previously stored vectors. Downstream code compares this with
/// `vault_documents.embedding_version` and re-embeds when they differ.
pub const EMBEDDING_SCHEMA_VERSION: u32 = 1;

/// Provider-agnostic embedding interface.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Provider family name (stable identifier — see `PROVIDER_*` constants).
    fn name(&self) -> &str;

    /// Concrete model identifier (e.g. `bge-m3`, `text-embedding-3-small`).
    /// Used for drift detection alongside `version()`.
    fn model(&self) -> &str {
        ""
    }

    /// Embedding schema version. Bump when semantics change in a way that
    /// invalidates previously stored vectors (dim, tokenizer, normalization).
    fn version(&self) -> u32 {
        EMBEDDING_SCHEMA_VERSION
    }

    /// Vector dimensionality (0 → noop / no embedding).
    fn dimensions(&self) -> usize;

    /// Embed a batch of texts.
    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Embed a single text. Default delegates to `embed`.
    async fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut results = self.embed(&[text]).await?;
        results
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Empty embedding result"))
    }
}

/// Factory for embedding providers. Recognised `provider` values:
///
/// * `"none"` / `""` / unknown → `NoopEmbedding`
/// * `"openai"` → OpenAI cloud
/// * `"openrouter"` → OpenRouter (OpenAI-compatible)
/// * `"local_fastembed"` → local BGE-M3 (requires `embedding-local` feature;
///   returns a fallback that errors on `embed()` if the feature is disabled —
///   callers should also use `doctor::embedding_provider_validation_error`
///   during config load to fail fast).
/// * `"custom:<url>"` → self-hosted OpenAI-compatible endpoint
///
/// Legacy callers pass `api_key + model + dims` — we accept these and forward
/// them to the concrete provider so old config schemas keep working.
pub fn create_embedding_provider(
    provider: &str,
    api_key: Option<&str>,
    model: &str,
    dims: usize,
) -> Box<dyn EmbeddingProvider> {
    let normalized = provider.trim();

    match normalized {
        PROVIDER_OPENAI => {
            let key = api_key.unwrap_or("");
            Box::new(OpenAiEmbedding::new(
                "https://api.openai.com",
                key,
                model,
                dims,
                PROVIDER_OPENAI,
            ))
        }
        "openrouter" => {
            let key = api_key.unwrap_or("");
            Box::new(OpenAiEmbedding::new(
                "https://openrouter.ai/api/v1",
                key,
                model,
                dims,
                PROVIDER_OPENAI,
            ))
        }
        PROVIDER_LOCAL_FASTEMBED => local_fastembed::create(model, dims),
        name if name.starts_with("custom:") => {
            let base_url = name.strip_prefix("custom:").unwrap_or("");
            let key = api_key.unwrap_or("");
            Box::new(custom_http::CustomHttpEmbedding::new(
                base_url, key, model, dims,
            ))
        }
        _ => Box::new(NoopEmbedding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_none_maps_to_noop() {
        let p = create_embedding_provider("none", None, "model", 1536);
        assert_eq!(p.name(), PROVIDER_NONE);
    }

    #[test]
    fn factory_empty_maps_to_noop() {
        let p = create_embedding_provider("", None, "model", 1536);
        assert_eq!(p.name(), PROVIDER_NONE);
    }

    #[test]
    fn factory_unknown_maps_to_noop() {
        let p = create_embedding_provider("cohere", None, "model", 1536);
        assert_eq!(p.name(), PROVIDER_NONE);
    }

    #[test]
    fn factory_openai_has_expected_metadata() {
        let p = create_embedding_provider("openai", Some("k"), "text-embedding-3-small", 1536);
        assert_eq!(p.name(), PROVIDER_OPENAI);
        assert_eq!(p.model(), "text-embedding-3-small");
        assert_eq!(p.dimensions(), 1536);
    }

    #[test]
    fn factory_openrouter_uses_openai_provider_family() {
        let p = create_embedding_provider(
            "openrouter",
            Some("sk-or-test"),
            "openai/text-embedding-3-small",
            1536,
        );
        assert_eq!(p.name(), PROVIDER_OPENAI);
        assert_eq!(p.model(), "openai/text-embedding-3-small");
    }

    #[test]
    fn factory_custom_maps_to_custom_http() {
        let p = create_embedding_provider("custom:http://localhost:1234", None, "model", 768);
        assert_eq!(p.name(), PROVIDER_CUSTOM_HTTP);
        assert_eq!(p.dimensions(), 768);
    }

    #[test]
    fn factory_local_fastembed_reports_provider_family() {
        let p = create_embedding_provider(PROVIDER_LOCAL_FASTEMBED, None, "bge-m3", 1024);
        assert_eq!(p.name(), PROVIDER_LOCAL_FASTEMBED);
        assert_eq!(p.dimensions(), 1024);
    }

    #[cfg(not(feature = "embedding-local"))]
    #[tokio::test]
    async fn local_fastembed_without_feature_errors_with_guidance() {
        let p = create_embedding_provider(PROVIDER_LOCAL_FASTEMBED, None, "bge-m3", 1024);
        let err = p.embed(&["hello"]).await.unwrap_err().to_string();
        assert!(
            err.contains("embedding-local"),
            "error must guide the operator to the feature flag, got: {err}"
        );
    }

    #[test]
    fn trait_default_version_matches_schema_constant() {
        let p = NoopEmbedding;
        assert_eq!(p.version(), EMBEDDING_SCHEMA_VERSION);
    }
}
