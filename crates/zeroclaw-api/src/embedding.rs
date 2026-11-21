//! Provider-neutral embedding contract.

use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingIdentity {
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
}

impl std::fmt::Display for EmbeddingIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{} ({} dims)",
            self.provider, self.model, self.dimensions
        )
    }
}

/// Trait for embedding model_providers — convert text to vectors
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// ModelProvider name
    fn name(&self) -> &str;

    /// Embedding dimensions
    fn dimensions(&self) -> usize;

    /// Embed a batch of texts into vectors
    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Embed a single text
    async fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.embed(&[text])
            .await?
            .pop()
            .ok_or_else(|| anyhow::Error::msg("Empty embedding result"))
    }
}
