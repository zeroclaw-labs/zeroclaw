//! Local ONNX embedding provider via fastembed.
//!
//! Uses BGE-small-en-v1.5 (384 dimensions) for fully offline embeddings.
//! Model downloads to ~/.cache/fastembed/ on first use (~130MB).

use crate::memory::embeddings::EmbeddingProvider;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Local ONNX embedding provider using fastembed.
pub struct OnnxEmbedding {
    model: Arc<Mutex<fastembed::TextEmbedding>>,
    dims: usize,
}

impl OnnxEmbedding {
    /// Create a new ONNX embedding provider with BGE-small-en-v1.5.
    ///
    /// First call downloads the model to `~/.cache/fastembed/` (~130MB).
    /// Subsequent calls load from cache.
    pub fn new() -> Result<Self> {
        let opts = fastembed::InitOptions::new(fastembed::EmbeddingModel::BGESmallENV15)
            .with_show_download_progress(true);
        let model = fastembed::TextEmbedding::try_new(opts)?;

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            dims: 384,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for OnnxEmbedding {
    fn name(&self) -> &str {
        "onnx"
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        let model = Arc::clone(&self.model);

        // fastembed is synchronous — run in blocking thread pool
        let embeddings = tokio::task::spawn_blocking(move || {
            let model = model.blocking_lock();
            model.embed(owned, None)
        })
        .await??;

        Ok(embeddings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onnx_embedding_name_and_dims() {
        // Skip if model download would be required in CI
        if std::env::var("CI").is_ok() {
            return;
        }

        if let Ok(provider) = OnnxEmbedding::new() {
            assert_eq!(provider.name(), "onnx");
            assert_eq!(provider.dimensions(), 384);
        }
    }
}
