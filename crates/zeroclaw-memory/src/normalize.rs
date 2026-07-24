//! Score normalization helpers for recall candidates.
//!
//! Recall fuses two score sources with incompatible scales: cosine
//! similarity (bounded to [0, 1]) and FTS5 BM25 (unbounded, negated to
//! higher-is-better at the search site). When both sources return rows,
//! `vector::hybrid_merge` already normalizes the keyword batch internally;
//! this module covers the keyword-only case so downstream consumers (the
//! injection relevance floor in particular) always see scores on the same
//! [0, 1] axis whenever the vector stage is live.

/// Normalize a batch of raw BM25-style scores to a bounded [0, 1] axis.
///
/// Scores are assumed to be higher-is-better by the time they reach this
/// helper (FTS5 BM25 is negated at the search site). Empty batches return
/// empty; all-zero batches return zero scores.
pub fn bm25_to_unit(raw: &[(String, f32)]) -> Vec<(String, f32)> {
    let max_score = raw.iter().map(|(_, score)| *score).fold(0.0_f32, f32::max);

    if max_score < f32::EPSILON {
        return raw.iter().map(|(id, _)| (id.clone(), 0.0)).collect();
    }

    raw.iter()
        .map(|(id, score)| (id.clone(), (*score / max_score).clamp(0.0, 1.0)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm25_to_unit_maps_best_score_to_one() {
        let normalized = bm25_to_unit(&[("a".into(), 2.0), ("b".into(), 4.0), ("c".into(), 1.0)]);

        assert_eq!(normalized[0], ("a".into(), 0.5));
        assert_eq!(normalized[1], ("b".into(), 1.0));
        assert_eq!(normalized[2], ("c".into(), 0.25));
    }

    #[test]
    fn bm25_to_unit_handles_empty_and_zero_batches() {
        assert!(bm25_to_unit(&[]).is_empty());
        assert_eq!(bm25_to_unit(&[("a".into(), 0.0)]), vec![("a".into(), 0.0)]);
    }
}
