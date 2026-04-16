//! Search-layer utilities: rank fusion and cross-encoder reranking.
//!
//! Split out in PR #4 so the retrieval pipeline stage is self-contained and
//! unit-testable. The key ideas:
//!
//! * **Fusion** (`fusion::k_way_rrf`): score-scale-agnostic merge of *any*
//!   number of ranked lists (multi-query × {vector, fts}, or multi-backend
//!   hits). The old 2-way merge in [`super::vector::rrf_merge`] is a special
//!   case — keep it around for backwards compatibility but prefer k-way for
//!   new call sites.
//! * **Reranker** (`rerank::Reranker`): cross-encoder pass over the top-N
//!   candidates. `NoopReranker` is the default (no behaviour change);
//!   `BgeReranker` runs BGE-reranker-v2-m3 via fastembed/ONNX when the
//!   `embedding-local` feature is compiled in. Off-path keeps the trait so
//!   callers can wire it up without `cfg` gates everywhere.

pub mod fusion;
pub mod rerank;

// `Ranker` and `NoopReranker` are the named "default" and "trait" entry
// points documented in the module header. They have no internal consumer
// today, but the public surface is intentional — keep them re-exported.
#[allow(unused_imports)]
pub use fusion::{k_way_rrf, Ranker, RrfSettings};
#[allow(unused_imports)]
pub use rerank::{NoopReranker, Reranker, RerankCandidate, RerankConfig as RerankRuntimeConfig};
