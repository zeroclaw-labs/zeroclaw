//! Local BGE-M3 embedder (on-device, zero-network).
//!
//! Load-bearing for the server-non-storage E2E encrypted sync patent:
//! embeddings must be computable without a cloud round-trip or the raw text
//! would need to leave the device. Provider string in the vault metadata
//! columns is `"local_fastembed"`.
//!
//! # Build modes
//!
//! This file is always compiled so the factory can return a typed provider
//! regardless of features, but the heavy ONNX runtime is opt-in:
//!
//! * `--features embedding-local`: `fastembed` crate is pulled in, BGE-M3 runs
//!   via ONNX Runtime against a model downloaded to
//!   `~/.moa/embedding-models/bge-m3/` (~1.1 GB on first run).
//! * default build: a stub provider returns an actionable error from
//!   `embed()` telling the operator to rebuild with `--features
//!   embedding-local`. Config validation (`doctor::embedding_provider_validation_error`)
//!   catches this earlier and surfaces a guided message at startup.
//!
//! The default-off posture keeps cold-start builds small and preserves the
//! project's binary-size goal (see `Cargo.toml` release profile). CI's
//! `nightly-all-features` lane exercises the real implementation.

use async_trait::async_trait;

use super::{EmbeddingProvider, EMBEDDING_SCHEMA_VERSION, PROVIDER_LOCAL_FASTEMBED};

/// Default model identifier. `BAAI/bge-m3` produces 1024-dim vectors with
/// strong multilingual performance — especially for Korean, which the project
/// optimises for.
pub const DEFAULT_MODEL: &str = "bge-m3";
/// Default embedding dimension for BGE-M3.
pub const DEFAULT_DIM: usize = 1024;

/// Factory — returns a [`LocalFastembedProvider`] when the feature is enabled,
/// otherwise a [`LocalFastembedStub`] that errors on `embed()` with guidance.
pub fn create(model: &str, dims: usize) -> Box<dyn EmbeddingProvider> {
    let model = if model.trim().is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        model.to_string()
    };
    let dims = if dims == 0 { DEFAULT_DIM } else { dims };

    #[cfg(feature = "embedding-local")]
    {
        match LocalFastembedProvider::try_new(&model, dims) {
            Ok(p) => Box::new(p),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "failed to initialise local_fastembed; falling back to stub"
                );
                Box::new(LocalFastembedStub::new(model, dims))
            }
        }
    }

    #[cfg(not(feature = "embedding-local"))]
    {
        Box::new(LocalFastembedStub::new(model, dims))
    }
}

/// Stub provider used when `embedding-local` is not compiled in. Reports the
/// expected metadata so downstream code (schema migration, config doctor) can
/// still reason about the intended model, but `embed()` returns an actionable
/// error instead of silently producing zero vectors.
pub struct LocalFastembedStub {
    model: String,
    dims: usize,
}

impl LocalFastembedStub {
    fn new(model: String, dims: usize) -> Self {
        Self { model, dims }
    }
}

#[async_trait]
impl EmbeddingProvider for LocalFastembedStub {
    fn name(&self) -> &str {
        PROVIDER_LOCAL_FASTEMBED
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn version(&self) -> u32 {
        EMBEDDING_SCHEMA_VERSION
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    async fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!(
            "local_fastembed is not available in this build; rebuild with \
             `--features embedding-local` to enable on-device BGE-M3 embedding"
        )
    }
}

// ── Real implementation (feature-gated) ───────────────────────────────

#[cfg(feature = "embedding-local")]
pub use real::LocalFastembedProvider;

#[cfg(not(feature = "embedding-local"))]
pub use LocalFastembedStub as LocalFastembedProvider;

#[cfg(feature = "embedding-local")]
mod real {
    use super::{EmbeddingProvider, EMBEDDING_SCHEMA_VERSION, PROVIDER_LOCAL_FASTEMBED};
    use async_trait::async_trait;
    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
    use parking_lot::Mutex;
    use std::{path::PathBuf, sync::Arc};

    /// BGE-M3 via `fastembed` + ONNX Runtime.
    ///
    /// Loads the model once at construction time into `~/.moa/embedding-models/
    /// bge-m3/`. The inner `TextEmbedding` holds ONNX session handles — we wrap
    /// it in a [`parking_lot::Mutex`] because the session is `!Sync` and
    /// `embed()` is re-entrant from async contexts.
    pub struct LocalFastembedProvider {
        model_id: String,
        dims: usize,
        inner: Arc<Mutex<TextEmbedding>>,
    }

    impl LocalFastembedProvider {
        /// Default cache directory for downloaded model weights
        /// (`~/.moa/embedding-models/`). Overridable via `MOA_EMBEDDING_CACHE`
        /// so test harnesses and sandboxed builds can redirect writes.
        fn cache_dir() -> PathBuf {
            std::env::var_os("MOA_EMBEDDING_CACHE")
                .map(PathBuf::from)
                .or_else(|| dirs_like_home().map(|home| home.join(".moa").join("embedding-models")))
                .unwrap_or_else(|| PathBuf::from(".moa-embedding-models"))
        }

        pub fn try_new(model: &str, dims: usize) -> anyhow::Result<Self> {
            let embedding_model = resolve_model(model)?;
            let cache_dir = Self::cache_dir();
            std::fs::create_dir_all(&cache_dir).ok();

            let options = InitOptions::new(embedding_model)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(true);
            let text_embedding = TextEmbedding::try_new(options).map_err(|e| {
                anyhow::anyhow!(
                    "failed to initialise fastembed ({model}); check network access to huggingface.co and disk space in ~/.moa/embedding-models: {e}"
                )
            })?;

            Ok(Self {
                model_id: model.to_string(),
                dims,
                inner: Arc::new(Mutex::new(text_embedding)),
            })
        }
    }

    #[async_trait]
    impl EmbeddingProvider for LocalFastembedProvider {
        fn name(&self) -> &str {
            PROVIDER_LOCAL_FASTEMBED
        }

        fn model(&self) -> &str {
            &self.model_id
        }

        fn version(&self) -> u32 {
            EMBEDDING_SCHEMA_VERSION
        }

        fn dimensions(&self) -> usize {
            self.dims
        }

        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }

            // Snapshot inputs — moved into the blocking task so we don't hold
            // a borrow across an await point.
            let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
            let inner = self.inner.clone();

            tokio::task::spawn_blocking(move || {
                let mut guard = inner.lock();
                let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
                guard
                    .embed(refs, None)
                    .map_err(|e| anyhow::anyhow!("fastembed embed failed: {e}"))
            })
            .await
            .map_err(|e| anyhow::anyhow!("fastembed blocking task panicked: {e}"))?
        }
    }

    fn resolve_model(name: &str) -> anyhow::Result<EmbeddingModel> {
        // Recognise the aliases we ship. Unknown names fall back to BGE-M3 so
        // bad config doesn't hard-fail at startup (operator can override via
        // config.toml once they've picked a valid model).
        match name.trim().to_ascii_lowercase().as_str() {
            "" | "bge-m3" | "baai/bge-m3" => Ok(EmbeddingModel::BGEM3),
            "bge-large-en-v1.5" | "baai/bge-large-en-v1.5" => Ok(EmbeddingModel::BGELargeENV15),
            "bge-small-en-v1.5" | "baai/bge-small-en-v1.5" => Ok(EmbeddingModel::BGESmallENV15),
            "multilingual-e5-large" | "intfloat/multilingual-e5-large" => {
                Ok(EmbeddingModel::MultilingualE5Large)
            }
            other => {
                tracing::warn!("unknown local_fastembed model '{other}'; falling back to BGE-M3");
                Ok(EmbeddingModel::BGEM3)
            }
        }
    }

    fn dirs_like_home() -> Option<std::path::PathBuf> {
        directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_returns_provider_with_expected_metadata() {
        let p = create("bge-m3", 1024);
        assert_eq!(p.name(), PROVIDER_LOCAL_FASTEMBED);
        assert_eq!(p.model(), "bge-m3");
        assert_eq!(p.dimensions(), 1024);
        assert_eq!(p.version(), EMBEDDING_SCHEMA_VERSION);
    }

    #[test]
    fn factory_fills_defaults_for_blank_model() {
        let p = create("", 0);
        assert_eq!(p.model(), DEFAULT_MODEL);
        assert_eq!(p.dimensions(), DEFAULT_DIM);
    }

    #[cfg(not(feature = "embedding-local"))]
    #[tokio::test]
    async fn stub_errors_with_feature_flag_guidance() {
        let p = create("bge-m3", 1024);
        let err = p.embed(&["hello"]).await.unwrap_err().to_string();
        assert!(err.contains("embedding-local"), "got: {err}");
        assert!(err.contains("rebuild"), "got: {err}");
    }

    #[cfg(not(feature = "embedding-local"))]
    #[tokio::test]
    async fn stub_embed_one_also_errors() {
        let p = create("bge-m3", 1024);
        assert!(p.embed_one("hi").await.is_err());
    }

    /// PR #1 실측 — Determinism check. Running the real BGE-M3 model
    /// twice on the same inputs must produce byte-identical vectors.
    /// Requires the model to be cached in ~/.moa/embedding-models/ (or
    /// $MOA_EMBEDDING_CACHE) — first run downloads ~1.1 GB from
    /// Hugging Face. CI's nightly-all-features lane exercises this
    /// after the cache warm-up step.
    #[cfg(feature = "embedding-local")]
    #[tokio::test]
    async fn embed_is_deterministic_for_identical_input() {
        let p = create("bge-m3", 1024);
        let a = p.embed(&["나는 변호사다", "hello world"]).await.unwrap();
        let b = p.embed(&["나는 변호사다", "hello world"]).await.unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 2);
        for (i, (va, vb)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(va.len(), 1024, "row {i} dim mismatch");
            assert_eq!(va, vb, "row {i} not deterministic");
        }
    }

    /// PR #1 실측 — Cross-lingual shape sanity: Korean and English
    /// inputs must produce equal-length vectors (BGE-M3 is multilingual,
    /// all outputs are 1024-dim) and distinct content must produce
    /// distinct vectors (rules out a pathological "all zeros" failure).
    #[cfg(feature = "embedding-local")]
    #[tokio::test]
    async fn embed_shape_and_distinctness_across_languages() {
        let p = create("bge-m3", 1024);
        let v = p
            .embed(&["주택임대차보호법 대항력", "housing tenant law", "완전히 다른 주제"])
            .await
            .unwrap();
        assert_eq!(v.len(), 3);
        assert!(v.iter().all(|vec| vec.len() == 1024));
        // Related Korean/English pair should be distinguishable from
        // the unrelated Korean sentence.
        assert_ne!(v[0], v[1]);
        assert_ne!(v[0], v[2]);
    }
}
