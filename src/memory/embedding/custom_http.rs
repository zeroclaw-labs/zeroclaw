//! User-supplied OpenAI-compatible embedding endpoint.
//!
//! Thin wrapper around [`OpenAiEmbedding`] that labels itself with the
//! `custom_http` provider family so downstream code can distinguish it from
//! direct OpenAI traffic (important for the PR #5 sync encryption path — a
//! `custom_http` embedding MUST be re-computed locally if the remote device
//! does not advertise the same endpoint).

use async_trait::async_trait;

use super::{EmbeddingProvider, OpenAiEmbedding, PROVIDER_CUSTOM_HTTP};

/// Embedder talking to a user-supplied OpenAI-compatible endpoint.
pub struct CustomHttpEmbedding {
    inner: OpenAiEmbedding,
}

impl CustomHttpEmbedding {
    pub fn new(base_url: &str, api_key: &str, model: &str, dims: usize) -> Self {
        Self {
            inner: OpenAiEmbedding::new(base_url, api_key, model, dims, PROVIDER_CUSTOM_HTTP),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for CustomHttpEmbedding {
    fn name(&self) -> &str {
        PROVIDER_CUSTOM_HTTP
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn version(&self) -> u32 {
        self.inner.version()
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.inner.embed(texts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_http_reports_custom_provider_family() {
        let p = CustomHttpEmbedding::new("http://tei.local:8080", "", "bge-m3", 1024);
        assert_eq!(p.name(), PROVIDER_CUSTOM_HTTP);
        assert_eq!(p.model(), "bge-m3");
        assert_eq!(p.dimensions(), 1024);
    }

    #[test]
    fn custom_http_accepts_empty_base_url() {
        // `custom:` prefix alone — degenerate, but must not panic.
        let p = CustomHttpEmbedding::new("", "", "model", 768);
        assert_eq!(p.name(), PROVIDER_CUSTOM_HTTP);
    }
}
