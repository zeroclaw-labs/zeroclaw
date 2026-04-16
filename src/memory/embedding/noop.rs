//! Keyword-only fallback — returns no vectors.

use async_trait::async_trait;

use super::{EmbeddingProvider, PROVIDER_NONE};

/// Placeholder embedder used when vector search is disabled. Always returns an
/// empty vector so callers fall back to FTS-only retrieval.
pub struct NoopEmbedding;

#[async_trait]
impl EmbeddingProvider for NoopEmbedding {
    fn name(&self) -> &str {
        PROVIDER_NONE
    }

    fn model(&self) -> &str {
        ""
    }

    fn dimensions(&self) -> usize {
        0
    }

    async fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_name_and_dims() {
        let p = NoopEmbedding;
        assert_eq!(p.name(), PROVIDER_NONE);
        assert_eq!(p.dimensions(), 0);
        assert!(p.model().is_empty());
    }

    #[tokio::test]
    async fn noop_embed_returns_empty_for_any_input() {
        let p = NoopEmbedding;
        assert!(p.embed(&[]).await.unwrap().is_empty());
        assert!(p.embed(&["hello"]).await.unwrap().is_empty());
        assert!(p.embed(&["a", "b", "c"]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn noop_embed_one_errors_on_empty_result() {
        let p = NoopEmbedding;
        assert!(p.embed_one("hello").await.is_err());
    }
}
