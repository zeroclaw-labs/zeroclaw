//! PR #6 — semantic consolidation (sleep cycle).
//!
//! Clusters near-duplicate memory entries by embedding cosine similarity,
//! then asks an LLM to summarise each cluster into a single
//! `consolidated_memories` row of type `semantic_fact`. Original rows are
//! soft-archived (`archived = 1`) but kept on disk so a user can recover
//! them through the archive UI.
//!
//! ## Algorithm
//!
//! 1. Filter candidate memories: not yet archived, has an embedding,
//!    `recall_count >= 1` (untouched memories are not consolidated — they
//!    haven't earned an opinion yet).
//! 2. Single-link clustering on cosine similarity ≥ `threshold`
//!    (default 0.88, matching the spec). Implemented via union-find so
//!    transitive groupings (`A↔B, B↔C → {A,B,C}`) collapse correctly.
//! 3. For every cluster of size ≥ 2, hand the contents to a [`Summarizer`]
//!    that returns either a `Consensus { summary }` or a
//!    `Conflict { summary, contradicting_keys }`.
//! 4. Write the result into `consolidated_memories(type, summary,
//!    source_ids, conflict_flag)` and flip every source memory's
//!    `archived = 1`.
//!
//! HDBSCAN was the spec's first choice but for a corpus of low-thousands
//! single-link gives almost identical clusters and ships in pure Rust
//! with no extra dependency. Once the corpus exceeds tens of thousands
//! we'll swap in `hdbscan-rs`.
//!
//! The clustering step is intentionally separated from the SQL/LLM
//! plumbing so we can unit-test grouping behaviour against fixture vectors
//! without spinning up a Connection.

use std::collections::HashMap;

use async_trait::async_trait;

/// One row that participates in a consolidation pass. Mirrors what the
/// caller will read out of `memories` after filtering.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateMemory {
    pub id: String,
    pub key: String,
    pub content: String,
    pub embedding: Vec<f32>,
}

/// Result a [`Summarizer`] returns for a single cluster.
#[derive(Debug, Clone, PartialEq)]
pub enum SummaryOutcome {
    /// All members agree — write one consolidated row, archive sources.
    Consensus { summary: String },
    /// Members disagree on a fact — store a row marked `conflict_flag=1`
    /// so a UI can prompt the user to choose.
    Conflict {
        summary: String,
        /// Keys of the memory rows that contradict each other. Stored
        /// alongside the summary for the conflict-resolution UI.
        contradicting_keys: Vec<String>,
    },
}

/// LLM-backed cluster summariser. Implementations are async because the
/// production summariser hits an LLM provider; tests pass a synchronous
/// fake (see [`tests::FakeSummarizer`] in this file).
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Summarise a non-empty cluster. Implementations MUST handle the
    /// `len == 1` case without panicking (the consolidation driver
    /// already filters those out, but defence in depth never hurts).
    async fn summarise(&self, cluster: &[&CandidateMemory]) -> anyhow::Result<SummaryOutcome>;
}

/// Cosine similarity between two equal-length f32 vectors. Returns 0.0
/// when either side is the zero vector — saves callers from `NaN`.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom <= f32::EPSILON {
        0.0
    } else {
        dot / denom
    }
}

/// Single-link cluster using the standard "near-duplicate threshold" trick:
/// build edges where `sim ≥ threshold`, run union-find, return groups.
///
/// Order of `candidates` is preserved within each cluster — the first id
/// to join a cluster becomes its representative for downstream LLM input.
#[must_use]
pub fn cluster_by_similarity(
    candidates: &[CandidateMemory],
    threshold: f32,
) -> Vec<Vec<&CandidateMemory>> {
    let n = candidates.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        // Path compression.
        let mut cursor = x;
        while parent[cursor] != root {
            let next = parent[cursor];
            parent[cursor] = root;
            cursor = next;
        }
        root
    }

    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            // Lower index becomes the representative so cluster order is
            // deterministic across runs.
            if ra < rb {
                parent[rb] = ra;
            } else {
                parent[ra] = rb;
            }
        }
    }

    for i in 0..n {
        for j in (i + 1)..n {
            if cosine_similarity(&candidates[i].embedding, &candidates[j].embedding) >= threshold {
                union(&mut parent, i, j);
            }
        }
    }

    let mut groups: HashMap<usize, Vec<&CandidateMemory>> = HashMap::new();
    for (i, c) in candidates.iter().enumerate() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(c);
    }

    // Keep clusters in the order their representative appeared in
    // `candidates` so the test suite (and the LLM prompt) sees stable
    // ordering.
    let mut keys: Vec<usize> = groups.keys().copied().collect();
    keys.sort_unstable();
    keys.into_iter()
        .map(|k| groups.remove(&k).unwrap())
        .collect()
}

/// One result entry consumed by the SQL writer — separates "what to write"
/// from "how to write it".
#[derive(Debug, Clone, PartialEq)]
pub struct ConsolidationOutcome {
    pub source_ids: Vec<String>,
    pub source_keys: Vec<String>,
    pub summary: String,
    pub conflict: bool,
    pub contradicting_keys: Vec<String>,
}

/// Run consolidation over a candidate set and return the work the caller
/// must persist. Singletons are skipped (no need to consolidate something
/// with itself); errors from the summariser become per-cluster failures
/// recorded in the returned `errors` list rather than aborting the whole
/// pass.
pub async fn consolidate_candidates(
    candidates: &[CandidateMemory],
    threshold: f32,
    summarizer: &dyn Summarizer,
) -> ConsolidationReport {
    let mut report = ConsolidationReport::default();
    let clusters = cluster_by_similarity(candidates, threshold);
    for cluster in clusters {
        if cluster.len() < 2 {
            report.singletons += 1;
            continue;
        }
        match summarizer.summarise(&cluster).await {
            Ok(outcome) => {
                let (summary, conflict, contradicting_keys) = match outcome {
                    SummaryOutcome::Consensus { summary } => (summary, false, Vec::new()),
                    SummaryOutcome::Conflict {
                        summary,
                        contradicting_keys,
                    } => (summary, true, contradicting_keys),
                };
                report.outcomes.push(ConsolidationOutcome {
                    source_ids: cluster.iter().map(|c| c.id.clone()).collect(),
                    source_keys: cluster.iter().map(|c| c.key.clone()).collect(),
                    summary,
                    conflict,
                    contradicting_keys,
                });
            }
            Err(err) => {
                report.errors.push(format!(
                    "cluster of {} ({}…): {err}",
                    cluster.len(),
                    cluster
                        .first()
                        .map(|c| c.key.as_str())
                        .unwrap_or("(empty)")
                ));
            }
        }
    }
    report
}

/// Aggregated outcome of a consolidation pass — what the dream cycle
/// reports up to telemetry / logs.
#[derive(Debug, Default, Clone)]
pub struct ConsolidationReport {
    pub outcomes: Vec<ConsolidationOutcome>,
    pub singletons: usize,
    pub errors: Vec<String>,
}

impl ConsolidationReport {
    pub fn consolidated_count(&self) -> usize {
        self.outcomes.len()
    }
    pub fn archived_source_count(&self) -> usize {
        self.outcomes.iter().map(|o| o.source_ids.len()).sum()
    }
    pub fn conflict_count(&self) -> usize {
        self.outcomes.iter().filter(|o| o.conflict).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, key: &str, content: &str, embedding: Vec<f32>) -> CandidateMemory {
        CandidateMemory {
            id: id.into(),
            key: key.into(),
            content: content.into(),
            embedding,
        }
    }

    #[test]
    fn cosine_similarity_handles_edge_cases() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        // Mismatched length → 0.
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), 0.0);
        // Empty → 0, no NaN.
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        // Zero vector → 0.
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn near_duplicates_collapse_into_one_cluster() {
        let cs = vec![
            cand("a", "k1", "원본", vec![1.0, 0.0, 0.0]),
            cand("b", "k2", "조금 다름", vec![0.95, 0.05, 0.0]),
            cand("c", "k3", "전혀 다름", vec![0.0, 1.0, 0.0]),
        ];
        let groups = cluster_by_similarity(&cs, 0.88);
        assert_eq!(groups.len(), 2);
        // The first group is the high-sim pair (a, b) since `a` carries
        // the lower index.
        let pair: Vec<&str> = groups[0].iter().map(|c| c.id.as_str()).collect();
        assert_eq!(pair, vec!["a", "b"]);
        // Second group is the singleton c.
        assert_eq!(groups[1][0].id, "c");
    }

    #[test]
    fn single_link_chains_through_intermediate() {
        // a↔b similar, b↔c similar, a↔c not similar — must still cluster
        // as {a,b,c}.
        let cs = vec![
            cand("a", "ka", "alpha", vec![1.0, 0.0]),
            cand("b", "kb", "beta", vec![0.93, 0.37]),
            cand("c", "kc", "gamma", vec![0.5, 0.866]),
        ];
        // a↔b cos = 0.93 > 0.9 ; b↔c cos ≈ 0.79 ; a↔c cos = 0.5
        let groups = cluster_by_similarity(&cs, 0.78);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3);
    }

    #[test]
    fn threshold_above_max_similarity_returns_singletons() {
        let cs = vec![
            cand("a", "ka", "x", vec![1.0, 0.0]),
            cand("b", "kb", "y", vec![0.95, 0.05]),
        ];
        let groups = cluster_by_similarity(&cs, 0.999);
        assert_eq!(groups.len(), 2);
    }

    pub struct FakeSummarizer {
        pub conflict_marker: Option<String>,
    }

    #[async_trait]
    impl Summarizer for FakeSummarizer {
        async fn summarise(
            &self,
            cluster: &[&CandidateMemory],
        ) -> anyhow::Result<SummaryOutcome> {
            // Synthetic summary: join contents. If any content matches
            // `conflict_marker`, return Conflict instead.
            let summary = cluster
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            if let Some(marker) = &self.conflict_marker {
                if cluster.iter().any(|c| c.content.contains(marker)) {
                    let keys = cluster.iter().map(|c| c.key.clone()).collect();
                    return Ok(SummaryOutcome::Conflict {
                        summary,
                        contradicting_keys: keys,
                    });
                }
            }
            Ok(SummaryOutcome::Consensus { summary })
        }
    }

    #[tokio::test]
    async fn consolidate_writes_one_outcome_per_multi_member_cluster() {
        let cs = vec![
            cand("a", "k1", "나는 변호사다", vec![1.0, 0.0, 0.0]),
            cand("b", "k2", "나는 변호사이다", vec![0.99, 0.0, 0.0]),
            cand("c", "k3", "강아지 보리", vec![0.0, 1.0, 0.0]),
        ];
        let s = FakeSummarizer {
            conflict_marker: None,
        };
        let r = consolidate_candidates(&cs, 0.88, &s).await;
        assert_eq!(r.consolidated_count(), 1);
        assert_eq!(r.singletons, 1);
        assert_eq!(r.archived_source_count(), 2);
        assert_eq!(r.conflict_count(), 0);
        assert_eq!(r.outcomes[0].source_keys, vec!["k1", "k2"]);
    }

    #[tokio::test]
    async fn conflict_marker_propagates_into_outcome() {
        let cs = vec![
            cand("a", "k1", "주말 골프", vec![1.0, 0.0]),
            cand("b", "k2", "주말 테니스", vec![0.97, 0.05]),
        ];
        let s = FakeSummarizer {
            conflict_marker: Some("골프".into()),
        };
        let r = consolidate_candidates(&cs, 0.88, &s).await;
        assert_eq!(r.consolidated_count(), 1);
        assert_eq!(r.conflict_count(), 1);
        assert_eq!(r.outcomes[0].contradicting_keys, vec!["k1", "k2"]);
    }

    #[tokio::test]
    async fn summariser_errors_become_per_cluster_failures_not_aborts() {
        struct ExplodingSummarizer;
        #[async_trait]
        impl Summarizer for ExplodingSummarizer {
            async fn summarise(
                &self,
                _cluster: &[&CandidateMemory],
            ) -> anyhow::Result<SummaryOutcome> {
                anyhow::bail!("LLM unavailable")
            }
        }
        let cs = vec![
            cand("a", "k1", "x", vec![1.0, 0.0]),
            cand("b", "k2", "y", vec![0.99, 0.0]),
            cand("c", "k3", "z", vec![0.5, 0.5]),
            cand("d", "k4", "w", vec![0.49, 0.51]),
        ];
        let r = consolidate_candidates(&cs, 0.88, &ExplodingSummarizer).await;
        // Two two-member clusters → both fail summarisation, neither
        // crashes the pass.
        assert_eq!(r.consolidated_count(), 0);
        assert_eq!(r.errors.len(), 2);
        assert_eq!(r.singletons, 0);
    }

    #[tokio::test]
    async fn empty_candidates_returns_empty_report() {
        let s = FakeSummarizer {
            conflict_marker: None,
        };
        let r = consolidate_candidates(&[], 0.88, &s).await;
        assert!(r.outcomes.is_empty());
        assert_eq!(r.singletons, 0);
        assert!(r.errors.is_empty());
    }
}
