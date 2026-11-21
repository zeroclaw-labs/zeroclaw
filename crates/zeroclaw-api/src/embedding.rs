//! Provider-neutral embedding contract.

use std::fmt;

/// Identity persisted alongside vector data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingIdentity {
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
}

impl fmt::Display for EmbeddingIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{} ({} dims)",
            self.provider, self.model, self.dimensions
        )
    }
}

/// A provider that turns text into vectors.
#[async_trait::async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Stable provider identifier.
    fn name(&self) -> &str;

    /// Vector dimensions returned by this provider.
    fn dimensions(&self) -> usize;

    /// Vector-space identity used by storage and refresh consumers. Concrete
    /// adapters include endpoint identity without exposing credentials.
    fn identity(&self) -> EmbeddingIdentity {
        EmbeddingIdentity {
            provider: self.name().to_owned(),
            model: String::new(),
            dimensions: self.dimensions(),
        }
    }

    /// Embed a batch of texts.
    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Embed one text using the batch operation.
    async fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.embed(&[text])
            .await?
            .pop()
            .ok_or_else(|| anyhow::Error::msg("Empty embedding result"))
    }
}
