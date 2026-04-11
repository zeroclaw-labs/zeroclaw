//! Heuristic importance scorer for non-LLM paths.
//!
//! Assigns importance scores (0.0–1.0) based on memory category and keyword
//! signals. Used when LLM-based consolidation is unavailable or as a fast
//! first-pass scorer.

use super::traits::MemoryCategory;

/// Base importance by category.
fn category_base_score(category: &MemoryCategory) -> f64 {
    match category {
        MemoryCategory::Core => 0.7,
        MemoryCategory::Daily => 0.3,
        MemoryCategory::Conversation => 0.2,
        MemoryCategory::Custom(_) => 0.4,
    }
}

/// Keyword boost: if the content contains high-signal keywords, bump importance.
fn keyword_boost(content: &str) -> f64 {
    const HIGH_SIGNAL_KEYWORDS: &[&str] = &[
        "decision",
        "always",
        "never",
        "important",
        "critical",
        "must",
        "requirement",
        "policy",
        "rule",
        "principle",
    ];

    let lowered = content.to_ascii_lowercase();
    let matches = HIGH_SIGNAL_KEYWORDS
        .iter()
        .filter(|kw| lowered.contains(**kw))
        .count();

    // Cap at +0.2
    (matches as f64 * 0.1).min(0.2)
}

/// Compute heuristic importance score for a memory entry.
pub fn compute_importance(content: &str, category: &MemoryCategory) -> f64 {
    let base = category_base_score(category);
    let boost = keyword_boost(content);
    (base + boost).min(1.0)
}

/// Compute final retrieval score incorporating importance and recency.
///
/// `hybrid_score`: raw retrieval score from FTS/vector (0.0–1.0)
/// `importance`: importance score (0.0–1.0)
/// `recency_decay`: recency factor (0.0–1.0, 1.0 = very recent)
pub fn weighted_final_score(hybrid_score: f64, importance: f64, recency_decay: f64) -> f64 {
    hybrid_score * 0.7 + importance * 0.2 + recency_decay * 0.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_category_has_high_base_score() {
        let score = compute_importance("some fact", &MemoryCategory::Core);
        assert!((score - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn conversation_category_has_low_base_score() {
        let score = compute_importance("chat message", &MemoryCategory::Conversation);
        assert!((score - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn keywords_boost_importance() {
        let score = compute_importance(
            "This is a critical decision that must always be followed",
            &MemoryCategory::Core,
        );
        // base 0.7 + boost for "critical", "decision", "must", "always" = 0.7 + 0.2 (capped) = 0.9
        assert!(score > 0.85);
    }

    #[test]
    fn boost_capped_at_point_two() {
        let score = compute_importance(
            "important critical decision rule policy must always never requirement principle",
            &MemoryCategory::Conversation,
        );
        // base 0.2 + max boost 0.2 = 0.4
        assert!((score - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn weighted_final_score_formula() {
        let score = weighted_final_score(1.0, 1.0, 1.0);
        assert!((score - 1.0).abs() < f64::EPSILON);

        let score = weighted_final_score(0.0, 0.0, 0.0);
        assert!(score.abs() < f64::EPSILON);

        let score = weighted_final_score(0.5, 0.5, 0.5);
        assert!((score - 0.5).abs() < f64::EPSILON);
    }
}
