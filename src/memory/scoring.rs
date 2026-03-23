// Multi-stage scoring pipeline for memory retrieval.
//
// Applied post-merge (after weighted vector+keyword combination),
// pre-decay. Each stage is a pure function for independent testability.

// Stage 1: BM25 sigmoid normalization
// SQLite FTS5 returns negative values (lower = more relevant).
// Sigmoid maps them into [0, 1].
pub const BM25_SIGMOID_SCALE: f64 = 5.0;

pub fn sigmoid_normalize(raw_bm25: f64) -> f64 {
    // raw_bm25 comes negative from FTS5 — invert before sigmoid.
    let x = -raw_bm25 / BM25_SIGMOID_SCALE;
    1.0 / (1.0 + x.exp())
}

// Stage 2: Additive fusion boost
// When an entry appears in both BM25 and vector results,
// it gets a boost — double evidence.
pub const FUSION_BOOST: f64 = 0.15;

pub fn apply_fusion_boost(score: f64, in_both: bool) -> f64 {
    if in_both {
        (score + FUSION_BOOST).min(1.0)
    } else {
        score
    }
}

// Stage 3: Soft min score filter
// Entries below the threshold are filtered out.
// "Soft" = default 0.3, configurable, no hard cut without config.
pub const DEFAULT_MIN_SCORE: f64 = 0.3;

pub fn passes_min_score(score: f64, min_score: f64) -> bool {
    score >= min_score
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stage 1
    #[test]
    fn sigmoid_maps_zero_to_half() {
        // raw_bm25 = 0.0 → sigmoid(0) = 0.5
        let result = sigmoid_normalize(0.0);
        assert!((result - 0.5).abs() < 1e-9);
    }

    #[test]
    fn sigmoid_output_is_bounded() {
        // For arbitrary FTS5 values, output must be in [0,1]
        for raw in &[-100.0, -10.0, -1.0, 0.0, 1.0, 10.0] {
            let s = sigmoid_normalize(*raw);
            assert!(s >= 0.0 && s <= 1.0, "out of bounds: {s}");
        }
    }

    // Stage 2
    #[test]
    fn fusion_boost_applied_when_in_both() {
        let boosted = apply_fusion_boost(0.6, true);
        assert!((boosted - 0.75).abs() < 1e-9);
    }

    #[test]
    fn fusion_boost_clamps_at_one() {
        let boosted = apply_fusion_boost(0.95, true);
        assert!((boosted - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fusion_boost_skipped_when_not_in_both() {
        let score = apply_fusion_boost(0.6, false);
        assert!((score - 0.6).abs() < 1e-9);
    }

    // Stage 3
    #[test]
    fn min_score_filter_passes_and_drops() {
        assert!(passes_min_score(0.5, DEFAULT_MIN_SCORE));
        assert!(!passes_min_score(0.1, DEFAULT_MIN_SCORE));
        assert!(passes_min_score(DEFAULT_MIN_SCORE, DEFAULT_MIN_SCORE)); // boundary
    }
}
