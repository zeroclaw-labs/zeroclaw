//! Genesis alignment tracking — measures drift from the original genesis prompt.
//!
//! Uses Jaccard similarity (word overlap) and recall (coverage of genesis words)
//! to quantify how much the agent's current soul has drifted from its original
//! genesis prompt.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Alignment score measuring drift from genesis prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlignmentScore {
    /// Jaccard similarity (intersection / union of word sets), 0.0..=1.0
    pub jaccard: f64,
    /// Recall of genesis words in current soul, 0.0..=1.0
    pub recall: f64,
    /// Combined alignment score (average of jaccard and recall), 0.0..=1.0
    pub combined: f64,
}

impl AlignmentScore {
    /// Compute alignment between a genesis prompt and current soul text.
    ///
    /// Both inputs are tokenized into lowercase word sets for comparison.
    pub fn compute(genesis: &str, current: &str) -> Self {
        let genesis_words = tokenize(genesis);
        let current_words = tokenize(current);

        if genesis_words.is_empty() && current_words.is_empty() {
            return Self {
                jaccard: 1.0,
                recall: 1.0,
                combined: 1.0,
            };
        }

        if genesis_words.is_empty() || current_words.is_empty() {
            return Self {
                jaccard: 0.0,
                recall: 0.0,
                combined: 0.0,
            };
        }

        let intersection = genesis_words.intersection(&current_words).count() as f64;
        let union = genesis_words.union(&current_words).count() as f64;

        let jaccard = intersection / union;
        let recall = intersection / genesis_words.len() as f64;
        let combined = f64::midpoint(jaccard, recall);

        Self {
            jaccard,
            recall,
            combined,
        }
    }

    /// Whether alignment is considered healthy (combined >= threshold).
    pub fn is_aligned(&self, threshold: f64) -> bool {
        self.combined >= threshold
    }
}

/// Tokenize text into a set of lowercase words.
fn tokenize(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|w| {
            w.to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_have_perfect_alignment() {
        let score =
            AlignmentScore::compute("You are an autonomous agent", "You are an autonomous agent");
        assert!((score.jaccard - 1.0).abs() < f64::EPSILON);
        assert!((score.recall - 1.0).abs() < f64::EPSILON);
        assert!((score.combined - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn completely_different_texts_have_zero_alignment() {
        let score = AlignmentScore::compute("alpha beta gamma", "delta epsilon zeta");
        assert!((score.jaccard).abs() < f64::EPSILON);
        assert!((score.recall).abs() < f64::EPSILON);
    }

    #[test]
    fn partial_overlap_has_intermediate_alignment() {
        let score =
            AlignmentScore::compute("you are an autonomous agent", "you are a helpful assistant");
        assert!(score.jaccard > 0.0);
        assert!(score.jaccard < 1.0);
        assert!(score.recall > 0.0);
        assert!(score.recall < 1.0);
    }

    #[test]
    fn empty_genesis_and_current_is_perfect() {
        let score = AlignmentScore::compute("", "");
        assert!((score.combined - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_genesis_non_empty_current_is_zero() {
        let score = AlignmentScore::compute("", "some text");
        assert!((score.combined).abs() < f64::EPSILON);
    }

    #[test]
    fn is_aligned_checks_threshold() {
        let score = AlignmentScore {
            jaccard: 0.8,
            recall: 0.9,
            combined: 0.85,
        };
        assert!(score.is_aligned(0.8));
        assert!(score.is_aligned(0.85));
        assert!(!score.is_aligned(0.9));
    }

    #[test]
    fn case_insensitive_comparison() {
        let score = AlignmentScore::compute("Hello World", "hello world");
        assert!((score.jaccard - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn punctuation_is_stripped() {
        let score = AlignmentScore::compute("hello, world!", "hello world");
        assert!((score.jaccard - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn alignment_score_serde_roundtrip() {
        let score = AlignmentScore::compute("test agent", "test agent helper");
        let json = serde_json::to_string(&score).unwrap();
        let parsed: AlignmentScore = serde_json::from_str(&json).unwrap();
        assert!((parsed.jaccard - score.jaccard).abs() < f64::EPSILON);
        assert!((parsed.recall - score.recall).abs() < f64::EPSILON);
    }
}
