//! Cross-encoder reranker abstraction.
//!
//! Runs after RRF fusion: take the top-N candidates, score each `(query,
//! candidate_text)` pair with a cross-encoder, and reorder by that score.
//! Cross-encoders consistently beat bi-encoder RRF on top-10 precision for
//! Korean queries (patent-relevant quality surface), at the cost of one
//! ONNX inference per candidate (~10 ms per pair on CPU).
//!
//! Implementations:
//!
//! * [`NoopReranker`] — identity pass (candidates returned unchanged). The
//!   default; used when `[search.rerank] enabled = false` in config or when
//!   the binary was built without `--features embedding-local`.
//! * [`BgeReranker`] — BGE-reranker-v2-m3 via `fastembed::TextRerank`. Only
//!   compiled when the `embedding-local` feature is enabled. Model weights
//!   are cached in `~/.moa/embedding-models/` (shared with the embedder).
//!
//! The trait is feature-agnostic so call sites (`SqliteMemory::recall_*`)
//! can accept `Arc<dyn Reranker>` without `cfg` gates. A stub returned from
//! the factory when the feature is off errors from `rerank()` with a clear
//! rebuild message.

use async_trait::async_trait;

/// Candidate carried into the reranker. `id` identifies the document so the
/// caller can merge the rerank scores back onto whatever full record type it
/// owns (`MemoryEntry`, `VaultDocument`, etc.). `text` is whatever content
/// the cross-encoder should score against the query — callers can choose
/// title, body, snippet, or concatenation.
#[derive(Debug, Clone)]
pub struct RerankCandidate {
    pub id: String,
    pub text: String,
    /// Score the candidate arrived with (from RRF or vector search). The
    /// reranker may use it as a tiebreaker, but usually overwrites.
    pub prior_score: f32,
}

/// Config for runtime rerank behaviour. Parsed from `[search.rerank]` in
/// `config.toml`.
#[derive(Debug, Clone)]
pub struct RerankConfig {
    pub enabled: bool,
    pub model: String,
    pub top_k_before: usize,
    pub top_k_after: usize,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "bge-reranker-v2-m3".into(),
            top_k_before: 50,
            top_k_after: 10,
        }
    }
}

#[async_trait]
pub trait Reranker: Send + Sync {
    /// Stable identifier (for metrics, logs).
    fn name(&self) -> &str;

    /// Cross-encoder pass. Candidates may be truncated (e.g. to
    /// `top_k_before`) by the caller before invocation; the reranker should
    /// score every candidate it is given and return them sorted by the new
    /// score, descending. The returned length may be ≤ input (some
    /// implementations drop candidates below a threshold) but never more.
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
    ) -> anyhow::Result<Vec<RerankCandidate>>;
}

/// Identity reranker — no-op. Ships as the default so callers can always
/// hold an `Arc<dyn Reranker>` without optionality bloat.
pub struct NoopReranker;

#[async_trait]
impl Reranker for NoopReranker {
    fn name(&self) -> &str {
        "noop"
    }

    async fn rerank(
        &self,
        _query: &str,
        candidates: Vec<RerankCandidate>,
    ) -> anyhow::Result<Vec<RerankCandidate>> {
        Ok(candidates)
    }
}

/// Factory — returns the best available reranker for the requested model.
/// When `embedding-local` is off, returns [`BgeRerankerStub`] which errors
/// with rebuild guidance from `rerank()` (mirrors `local_fastembed.rs`).
pub fn create_reranker(model: &str) -> std::sync::Arc<dyn Reranker> {
    #[cfg(feature = "embedding-local")]
    {
        match real::BgeReranker::try_new(model) {
            Ok(r) => std::sync::Arc::new(r),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "failed to initialise BGE reranker; falling back to noop"
                );
                std::sync::Arc::new(BgeRerankerStub {
                    model: model.to_string(),
                })
            }
        }
    }

    #[cfg(not(feature = "embedding-local"))]
    {
        std::sync::Arc::new(BgeRerankerStub {
            model: model.to_string(),
        })
    }
}

/// Stub used when `embedding-local` is not compiled in OR when real
/// initialisation failed at runtime. Returns a guided error rather than
/// silently passing candidates through — silent degrade here would
/// invalidate the "rerank on improves accuracy ≥5pt" acceptance criterion
/// without telling the operator why.
pub struct BgeRerankerStub {
    model: String,
}

#[async_trait]
impl Reranker for BgeRerankerStub {
    fn name(&self) -> &str {
        "bge-reranker-stub"
    }

    async fn rerank(
        &self,
        _query: &str,
        _candidates: Vec<RerankCandidate>,
    ) -> anyhow::Result<Vec<RerankCandidate>> {
        anyhow::bail!(
            "BGE reranker '{}' not available in this build; rebuild with \
             `--features embedding-local` to enable cross-encoder reranking",
            self.model
        )
    }
}

// Re-export the real type so call sites can name it without `cfg` gates.
#[cfg(feature = "embedding-local")]
pub use real::BgeReranker;
#[cfg(not(feature = "embedding-local"))]
pub use BgeRerankerStub as BgeReranker;

#[cfg(feature = "embedding-local")]
mod real {
    use super::{async_trait, RerankCandidate, Reranker};
    use fastembed::{RerankInitOptions, RerankerModel, TextRerank};
    use parking_lot::Mutex;
    use std::{path::PathBuf, sync::Arc};

    /// BGE-reranker via `fastembed` + ONNX. Thread-safe wrapper around the
    /// inner `TextRerank` (which holds a non-`Sync` ONNX session).
    pub struct BgeReranker {
        model_id: String,
        inner: Arc<Mutex<TextRerank>>,
    }

    impl BgeReranker {
        fn cache_dir() -> PathBuf {
            std::env::var_os("MOA_EMBEDDING_CACHE")
                .map(PathBuf::from)
                .or_else(|| {
                    directories::UserDirs::new()
                        .map(|u| u.home_dir().join(".moa").join("embedding-models"))
                })
                .unwrap_or_else(|| PathBuf::from(".moa-embedding-models"))
        }

        pub fn try_new(model: &str) -> anyhow::Result<Self> {
            let reranker_model = resolve_model(model);
            let cache_dir = Self::cache_dir();
            std::fs::create_dir_all(&cache_dir).ok();

            let options = RerankInitOptions::new(reranker_model)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(true);
            let inner = TextRerank::try_new(options).map_err(|e| {
                anyhow::anyhow!(
                    "failed to initialise BGE reranker ({model}); check network access and disk space in ~/.moa/embedding-models: {e}"
                )
            })?;

            Ok(Self {
                model_id: model.to_string(),
                inner: Arc::new(Mutex::new(inner)),
            })
        }
    }

    #[async_trait]
    impl Reranker for BgeReranker {
        fn name(&self) -> &str {
            "bge-reranker"
        }

        async fn rerank(
            &self,
            query: &str,
            candidates: Vec<RerankCandidate>,
        ) -> anyhow::Result<Vec<RerankCandidate>> {
            if candidates.is_empty() {
                return Ok(candidates);
            }
            let query = query.to_string();
            let inner = self.inner.clone();
            let model_id = self.model_id.clone();

            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<RerankCandidate>> {
                // fastembed 5.8 expects `AsRef<[&String]>` — the API
                // tightened in 5.13. We keep the contract portable by
                // cloning into owned Strings; the refs happen inline.
                let owned: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
                let doc_refs: Vec<&String> = owned.iter().collect();
                let mut guard = inner.lock();
                let scored = guard
                    .rerank(&query, doc_refs, true, None)
                    .map_err(|e| anyhow::anyhow!("fastembed rerank ({model_id}) failed: {e}"))?;

                // `scored` is sorted desc by score with original `index` attached.
                let mut out = Vec::with_capacity(scored.len());
                for s in scored {
                    if s.index < candidates.len() {
                        let mut c = candidates[s.index].clone();
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            c.prior_score = s.score as f32;
                        }
                        out.push(c);
                    }
                }
                Ok(out)
            })
            .await
            .map_err(|e| anyhow::anyhow!("rerank blocking task panicked: {e}"))?
        }
    }

    fn resolve_model(name: &str) -> RerankerModel {
        match name.trim().to_ascii_lowercase().as_str() {
            "" | "bge-reranker-v2-m3" | "baai/bge-reranker-v2-m3" => RerankerModel::BGERerankerV2M3,
            "bge-reranker-base" | "baai/bge-reranker-base" => RerankerModel::BGERerankerBase,
            "jina-reranker-v2-base-multilingual" => RerankerModel::JINARerankerV2BaseMultiligual,
            other => {
                tracing::warn!(
                    "unknown BGE reranker model '{other}'; falling back to bge-reranker-v2-m3"
                );
                RerankerModel::BGERerankerV2M3
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, text: &str, prior: f32) -> RerankCandidate {
        RerankCandidate {
            id: id.into(),
            text: text.into(),
            prior_score: prior,
        }
    }

    #[tokio::test]
    async fn noop_preserves_order_and_scores() {
        let r = NoopReranker;
        let input = vec![
            cand("a", "apple pie", 0.9),
            cand("b", "banana bread", 0.8),
            cand("c", "carrot cake", 0.7),
        ];
        let out = r.rerank("dessert", input.clone()).await.unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[2].id, "c");
        assert!((out[0].prior_score - 0.9).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn noop_handles_empty_input() {
        let r = NoopReranker;
        let out = r.rerank("q", Vec::new()).await.unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn default_rerank_config_is_disabled_with_bge_defaults() {
        let cfg = RerankConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.model, "bge-reranker-v2-m3");
        assert_eq!(cfg.top_k_before, 50);
        assert_eq!(cfg.top_k_after, 10);
    }

    #[cfg(not(feature = "embedding-local"))]
    #[tokio::test]
    async fn factory_without_feature_returns_stub_that_errors() {
        let r = create_reranker("bge-reranker-v2-m3");
        let err = r
            .rerank("q", vec![cand("a", "t", 0.5)])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("embedding-local"), "got: {err}");
        assert!(err.contains("rebuild"), "got: {err}");
    }

    #[cfg(not(feature = "embedding-local"))]
    #[tokio::test]
    async fn stub_reports_model_name_in_error() {
        let r = create_reranker("custom-model-xyz");
        let err = r.rerank("q", vec![]).await.unwrap_err().to_string();
        assert!(err.contains("custom-model-xyz"), "got: {err}");
    }
}
