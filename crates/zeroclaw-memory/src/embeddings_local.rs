//! Local ONNX embedding provider.
//!
//! The HTTP providers in [`super::embeddings`] bill per stored memory and add a
//! network round trip to every write and recall. They also cannot serve a model
//! that is not on the vendor's menu, which rules out the language-specific
//! encoders that matter when the agent does not converse in English.
//!
//! This provider runs the model in-process instead: no key, no per-memory cost,
//! no network. It is behind the `memory-local-embeddings` feature because the
//! ONNX runtime is a heavy dependency that most deployments do not need.

use super::embeddings::EmbeddingProvider;
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use parking_lot::Mutex;
use std::sync::Arc;

/// Embeds text with a locally-run ONNX model.
pub struct LocalEmbedding {
    /// `fastembed` inference is synchronous and `&mut`, so calls are serialized
    /// behind a mutex and moved off the async runtime.
    model: Arc<Mutex<TextEmbedding>>,
    model_name: String,
    dimensions: usize,
}

impl std::fmt::Debug for LocalEmbedding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalEmbedding")
            .field("model", &self.model_name)
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

impl LocalEmbedding {
    /// Load `model_name`, downloading it on first use and caching it thereafter.
    ///
    /// `cache_dir` overrides where the weights live; `None` uses the
    /// `fastembed` default. Fails rather than falling back to a different model,
    /// because embeddings from two different models are not comparable and a
    /// silent substitution would corrupt every stored vector.
    pub fn new(model_name: &str, cache_dir: Option<&std::path::Path>) -> anyhow::Result<Self> {
        let model = Self::parse_model(model_name)?;
        let dimensions = Self::dimensions_for(model.clone())?;

        let mut options = TextInitOptions::new(model);
        if let Some(dir) = cache_dir {
            options = options.with_cache_dir(dir.to_path_buf());
        }

        let embedding = TextEmbedding::try_new(options).map_err(|e| {
            anyhow::Error::msg(format!(
                "failed to load local embedding model {model_name}: {e}"
            ))
        })?;

        Ok(Self {
            model: Arc::new(Mutex::new(embedding)),
            model_name: model_name.to_string(),
            dimensions,
        })
    }

    /// Resolve a configured name against the models this build supports.
    ///
    /// The error lists what is available: an unknown name is almost always a
    /// typo or a model from a different provider's catalogue.
    fn parse_model(name: &str) -> anyhow::Result<EmbeddingModel> {
        let wanted = name.trim().to_lowercase();
        TextEmbedding::list_supported_models()
            .into_iter()
            .find(|info| {
                info.model_code.to_lowercase() == wanted
                    || format!("{:?}", info.model).to_lowercase() == wanted
            })
            .map(|info| info.model)
            .ok_or_else(|| {
                let mut available: Vec<String> = TextEmbedding::list_supported_models()
                    .into_iter()
                    .map(|info| info.model_code)
                    .collect();
                available.sort();
                anyhow::Error::msg(format!(
                    "unknown local embedding model '{name}'; available: {}",
                    available.join(", ")
                ))
            })
    }

    /// Read the model's dimension from its own metadata rather than trusting a
    /// configured value: a mismatch would silently produce unusable vectors.
    fn dimensions_for(model: EmbeddingModel) -> anyhow::Result<usize> {
        TextEmbedding::list_supported_models()
            .into_iter()
            .find(|info| info.model == model)
            .map(|info| info.dim)
            .ok_or_else(|| anyhow::Error::msg("model reports no dimension"))
    }
}

#[async_trait]
impl EmbeddingProvider for LocalEmbedding {
    fn name(&self) -> &str {
        "local"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let owned: Vec<String> = texts.iter().map(|t| (*t).to_string()).collect();
        let model = Arc::clone(&self.model);

        // ONNX inference is CPU-bound and blocking; running it on the async
        // runtime would stall every other task on the worker thread.
        let vectors = tokio::task::spawn_blocking(move || {
            let mut guard = model.lock();
            guard.embed(owned, None)
        })
        .await
        .map_err(|e| anyhow::Error::msg(format!("local embedding task panicked: {e}")))?
        .map_err(|e| anyhow::Error::msg(format!("local embedding failed: {e}")))?;

        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_model_names_are_rejected_with_the_available_list() {
        // Falling back to a default model would mix incompatible vectors into
        // one index, so an unknown name must fail loudly.
        let err = LocalEmbedding::parse_model("text-embedding-3-small")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown local embedding model"));
        assert!(
            err.contains("available:"),
            "error should list supported models: {err}"
        );
    }

    #[test]
    fn model_names_resolve_case_insensitively() {
        // Config files are hand-written; casing should not decide whether the
        // agent has a memory index.
        let canonical = TextEmbedding::list_supported_models()
            .into_iter()
            .next()
            .expect("fastembed ships at least one model");
        assert_eq!(
            LocalEmbedding::parse_model(&canonical.model_code.to_uppercase()).unwrap(),
            canonical.model.clone()
        );
    }

    #[test]
    fn dimensions_come_from_model_metadata() {
        // The stored vector width must match the model, not a config guess.
        for info in TextEmbedding::list_supported_models().into_iter().take(5) {
            assert_eq!(
                LocalEmbedding::dimensions_for(info.model.clone()).unwrap(),
                info.dim,
                "dimension mismatch for {}",
                info.model_code
            );
        }
    }
}
