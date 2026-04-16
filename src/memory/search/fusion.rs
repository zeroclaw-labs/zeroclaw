//! K-way Reciprocal Rank Fusion (RRF).
//!
//! Given N ranked candidate lists (e.g. vector search over 3 query
//! variations plus FTS over the same 3 variations → N = 6), compute a
//! combined score that respects rank position in each source while being
//! invariant to score scale (cosine vs. BM25 vs. arbitrary).
//!
//! Formula: `rrf(doc) = Σ_i 1 / (k + rank_i(doc))` where rank is 1-based and
//! missing rankers contribute 0. The classic k = 60 (Cormack et al., 2009)
//! is a robust default — tuning k typically moves top-10 recall by <1%.
//!
//! Compared to the older 2-way merge in `memory::vector::rrf_merge` this
//! function preserves per-ranker rank even when the same document appears
//! in multiple rankers: that's the whole reason multi-query RRF beats
//! "flatten-then-dedup → single-ranker RRF".

use std::collections::HashMap;

/// A single ranked list produced by one retriever (or one query).
/// Entries must be sorted by the retriever's own relevance score,
/// descending. The scores themselves are preserved only so downstream code
/// can surface them for debugging — RRF ignores them.
pub type Ranker<'a> = &'a [(String, f32)];

/// Tunables for [`k_way_rrf`].
#[derive(Debug, Clone, Copy)]
pub struct RrfSettings {
    /// Cormack-et-al. k. Default 60.
    pub k: f32,
    /// Maximum length of the returned list.
    pub limit: usize,
}

impl Default for RrfSettings {
    fn default() -> Self {
        Self {
            k: 60.0,
            limit: 10,
        }
    }
}

/// One item produced by [`k_way_rrf`]. `source_count` is how many rankers
/// contained this id — useful for "only show items supported by ≥2 sources"
/// filtering in future callers.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedResult {
    pub id: String,
    pub score: f32,
    pub source_count: u32,
}

/// Fuse an arbitrary number of ranked lists.
///
/// # Contract
/// * Each ranker's position in the outer slice is irrelevant to the score
///   (RRF is commutative across rankers).
/// * Duplicates within a single ranker are collapsed by best rank (earliest
///   position) — callers passing already-deduped lists pay nothing.
/// * Empty rankers contribute nothing; an all-empty input returns `vec![]`.
pub fn k_way_rrf(rankers: &[Ranker<'_>], settings: RrfSettings) -> Vec<FusedResult> {
    if rankers.is_empty() || settings.limit == 0 {
        return Vec::new();
    }

    let mut acc: HashMap<String, (f32, u32)> = HashMap::new();

    for ranker in rankers {
        // Collapse intra-ranker duplicates by best (earliest) rank.
        let mut seen_in_this_ranker: HashMap<&str, usize> = HashMap::new();
        for (rank_0, (id, _score)) in ranker.iter().enumerate() {
            seen_in_this_ranker
                .entry(id.as_str())
                .or_insert(rank_0 + 1);
        }

        for (id_ref, rank_1based) in seen_in_this_ranker {
            #[allow(clippy::cast_precision_loss)]
            let contribution = 1.0 / (settings.k + rank_1based as f32);
            let entry = acc.entry(id_ref.to_string()).or_insert((0.0, 0));
            entry.0 += contribution;
            entry.1 += 1;
        }
    }

    let mut results: Vec<FusedResult> = acc
        .into_iter()
        .map(|(id, (score, source_count))| FusedResult {
            id,
            score,
            source_count,
        })
        .collect();

    results.sort_by(|a, b| {
        // Total score desc; tiebreak by more sources covering it; then
        // stable lex order on id so the test suite is deterministic.
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.source_count.cmp(&a.source_count))
            .then_with(|| a.id.cmp(&b.id))
    });
    results.truncate(settings.limit);
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(ids: &[&str]) -> Vec<(String, f32)> {
        // Score is irrelevant to RRF but we feed descending numbers so the
        // fixtures look like realistic ranked output.
        ids.iter()
            .enumerate()
            .map(|(i, id)| ((*id).to_string(), 1.0 - i as f32 * 0.01))
            .collect()
    }

    #[test]
    fn empty_input_returns_empty() {
        let out = k_way_rrf(&[], RrfSettings::default());
        assert!(out.is_empty());
    }

    #[test]
    fn single_ranker_preserves_order() {
        let r = mk(&["a", "b", "c"]);
        let out = k_way_rrf(&[&r], RrfSettings::default());
        assert_eq!(
            out.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        // Source count is 1 for all.
        assert!(out.iter().all(|f| f.source_count == 1));
    }

    #[test]
    fn two_rankers_boost_shared_documents() {
        let r1 = mk(&["a", "b", "c"]);
        let r2 = mk(&["c", "a", "d"]);
        let out = k_way_rrf(&[&r1, &r2], RrfSettings::default());
        // a: 1/61 + 1/62 ≈ 0.0325
        // c: 1/63 + 1/61 ≈ 0.0323
        // b: 1/62 ≈ 0.0161
        // d: 1/63 ≈ 0.0159
        let ordered: Vec<_> = out.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ordered, vec!["a", "c", "b", "d"]);
        assert_eq!(out[0].source_count, 2);
        assert_eq!(out[1].source_count, 2);
        assert_eq!(out[2].source_count, 1);
    }

    #[test]
    fn rrf_is_invariant_to_score_scale() {
        // r1 uses cosine-like small positive scores; r2 uses BM25-like
        // unbounded scores. RRF should not care.
        let r1 = vec![
            ("x".into(), 0.92f32),
            ("y".into(), 0.11f32),
            ("z".into(), 0.05f32),
        ];
        let r2 = vec![
            ("y".into(), 42.0f32),
            ("z".into(), 15.0f32),
            ("x".into(), 3.0f32),
        ];
        let out = k_way_rrf(&[&r1, &r2], RrfSettings::default());
        // x: 1/61 + 1/63 ; y: 1/62 + 1/61 ; z: 1/63 + 1/62
        // y dominates because it's rank-1 in r2 and rank-2 in r1.
        assert_eq!(out[0].id, "y");
    }

    #[test]
    fn repeated_low_rank_support_beats_single_high_rank_hit() {
        // "broad" appears at rank-3 in six distinct rankers whose top-2
        // spots are occupied by unique-per-ranker noise. "solo" only shows
        // up at rank-1 in a seventh ranker.
        //   broad: 6 × 1/(60+3) ≈ 0.0952
        //   solo : 1 × 1/(60+1) ≈ 0.0164
        // No other id accumulates more than a single contribution, so none
        // of the rank-1 noise can overtake "broad".
        let r_solo = vec![("solo".to_string(), 1.0f32)];
        let r1 = mk(&["n1a", "n1b", "broad"]);
        let r2 = mk(&["n2a", "n2b", "broad"]);
        let r3 = mk(&["n3a", "n3b", "broad"]);
        let r4 = mk(&["n4a", "n4b", "broad"]);
        let r5 = mk(&["n5a", "n5b", "broad"]);
        let r6 = mk(&["n6a", "n6b", "broad"]);
        let rankers = [
            &r_solo[..],
            &r1[..],
            &r2[..],
            &r3[..],
            &r4[..],
            &r5[..],
            &r6[..],
        ];
        let out = k_way_rrf(&rankers, RrfSettings::default());
        assert_eq!(out[0].id, "broad", "6× rank-3 should dominate 1× rank-1");
        assert_eq!(out[0].source_count, 6);
    }

    #[test]
    fn intra_ranker_duplicates_use_best_rank_only() {
        // If the same id somehow appears twice in one ranker (dirty input),
        // we don't want to award it *twice* the contribution — only the
        // best (earliest) rank counts.
        let r = vec![
            ("dup".into(), 0.9f32),
            ("other".into(), 0.8f32),
            ("dup".into(), 0.7f32),
        ];
        let r_other = mk(&["other", "dup"]);
        let out = k_way_rrf(&[&r, &r_other], RrfSettings::default());
        // "dup": rank 1 in r (not 1 + 3) + rank 2 in r_other = 1/61 + 1/62
        // "other": rank 2 in r + rank 1 in r_other = 1/62 + 1/61
        // They must tie on score; tie breaks by source_count (both 2); then lex.
        assert_eq!(out.len(), 2);
        // Both appeared in both rankers, so source_count = 2 each.
        assert!(out.iter().all(|f| f.source_count == 2));
        assert!((out[0].score - out[1].score).abs() < 1e-6);
    }

    #[test]
    fn limit_truncates_but_ordering_is_stable() {
        let r = mk(&["a", "b", "c", "d", "e"]);
        let settings = RrfSettings { k: 60.0, limit: 3 };
        let out = k_way_rrf(&[&r], settings);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn empty_ranker_inside_list_is_ignored() {
        let r1 = mk(&["a", "b"]);
        let empty: Vec<(String, f32)> = Vec::new();
        let r2 = mk(&["b", "c"]);
        let out = k_way_rrf(&[&r1, &empty, &r2], RrfSettings::default());
        assert_eq!(out[0].id, "b");
        assert_eq!(out[0].source_count, 2);
    }

    #[test]
    fn zero_limit_returns_empty() {
        let r = mk(&["a", "b"]);
        let out = k_way_rrf(&[&r], RrfSettings { k: 60.0, limit: 0 });
        assert!(out.is_empty());
    }

    #[test]
    fn k_parameter_is_respected() {
        // Different k shifts the score magnitude but not the order within a
        // single ranker.
        let r = mk(&["a", "b", "c"]);
        let out_60 = k_way_rrf(&[&r], RrfSettings { k: 60.0, limit: 3 });
        let out_10 = k_way_rrf(&[&r], RrfSettings { k: 10.0, limit: 3 });
        assert_eq!(
            out_60.iter().map(|f| f.id.clone()).collect::<Vec<_>>(),
            out_10.iter().map(|f| f.id.clone()).collect::<Vec<_>>(),
        );
        // But the scores must differ — k=10 concentrates weight on top ranks.
        assert!(out_10[0].score > out_60[0].score);
    }
}
