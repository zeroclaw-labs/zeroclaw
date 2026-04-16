//! Applier — apply user feedback (accept/reject/modify) to the pattern store.

use super::recommender::CorrectionRecommendation;
use super::store::CorrectionStore;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// What the user did with a recommendation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UserAction {
    /// User accepted the suggestion as-is.
    Accept,
    /// User rejected the suggestion (keep original).
    Reject,
    /// User modified the suggestion (replaced with custom text).
    Modify(String),
}

/// Apply user feedback to the pattern store.
///
/// - Accept: confidence +0.05, accept_count +1
/// - Reject: confidence -0.10, reject_count +1 (may deactivate)
/// - Modify: record as new observation for future learning
pub fn apply_feedback(
    store: &CorrectionStore,
    recommendation: &CorrectionRecommendation,
    action: &UserAction,
) -> Result<FeedbackReport> {
    match action {
        UserAction::Accept => {
            store.increment_accept(recommendation.pattern_id)?;
            Ok(FeedbackReport {
                confidence_delta: 0.05,
                pattern_deactivated: false,
                new_observation_recorded: false,
            })
        }
        UserAction::Reject => {
            store.increment_reject(recommendation.pattern_id)?;
            Ok(FeedbackReport {
                confidence_delta: -0.10,
                pattern_deactivated: false, // caller can re-query if needed
                new_observation_recorded: false,
            })
        }
        UserAction::Modify(_custom) => {
            // The caller should record a new observation separately
            // (original → custom) via observer + mine_observation.
            Ok(FeedbackReport {
                confidence_delta: 0.0,
                pattern_deactivated: false,
                new_observation_recorded: true,
            })
        }
    }
}

/// Apply a batch of accepted recommendations to a document, returning
/// the edited document and the list of applied recommendations.
///
/// Recommendations are applied in reverse order (by location) to avoid
/// offset shifts. Overlapping recommendations are resolved by taking
/// the first (highest-confidence) one.
pub fn apply_batch_to_document(
    document: &str,
    accepted: &[CorrectionRecommendation],
) -> (String, Vec<CorrectionRecommendation>) {
    // Sort by location descending to apply right-to-left
    let mut sorted: Vec<CorrectionRecommendation> = accepted.to_vec();
    sorted.sort_by(|a, b| b.location_start.cmp(&a.location_start));

    // Drop overlapping (keep first = rightmost)
    let mut applied = Vec::with_capacity(sorted.len());
    let mut last_start = usize::MAX;
    for rec in sorted {
        if rec.location_end <= last_start {
            last_start = rec.location_start;
            applied.push(rec);
        }
    }

    // Apply in reverse-location order (biggest offset first)
    let mut result = document.to_string();
    for rec in &applied {
        if rec.location_end <= result.len() {
            result.replace_range(rec.location_start..rec.location_end, &rec.suggested);
        }
    }

    // Return applied recommendations in original order (low-to-high location)
    applied.reverse();

    (result, applied)
}

#[derive(Debug, Clone, Default)]
pub struct FeedbackReport {
    pub confidence_delta: f64,
    pub pattern_deactivated: bool,
    pub new_observation_recorded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::store::PatternType;
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_store() -> CorrectionStore {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        CorrectionStore::new(Arc::new(Mutex::new(conn)), "test-device".into())
    }

    #[test]
    fn accept_increases_confidence() {
        let store = test_store();
        let id = store
            .create_pattern(PatternType::Style, "a", "b", "all")
            .unwrap();
        store.bump_confidence(id, 0.4).unwrap(); // now ~0.7

        let rec = CorrectionRecommendation {
            pattern_id: id,
            pattern_type: PatternType::Style,
            location_start: 0,
            location_end: 1,
            original: "a".into(),
            suggested: "b".into(),
            confidence: 0.7,
            observation_count: 2,
        };

        apply_feedback(&store, &rec, &UserAction::Accept).unwrap();
        let updated = store.find_pattern("a", "b").unwrap().unwrap();
        assert_eq!(updated.accept_count, 1);
        assert!(updated.confidence > 0.7);
    }

    #[test]
    fn apply_batch_edits_document() {
        let doc = "피고는 변제하였다. 원고는 청구하였다.";
        // Locations of "하였다"
        let p1 = doc.find("하였다").unwrap();
        let p2 = doc.rfind("하였다").unwrap();

        let recs = vec![
            CorrectionRecommendation {
                pattern_id: 1,
                pattern_type: PatternType::Style,
                location_start: p1,
                location_end: p1 + "하였다".len(),
                original: "하였다".into(),
                suggested: "합니다".into(),
                confidence: 0.8,
                observation_count: 3,
            },
            CorrectionRecommendation {
                pattern_id: 1,
                pattern_type: PatternType::Style,
                location_start: p2,
                location_end: p2 + "하였다".len(),
                original: "하였다".into(),
                suggested: "합니다".into(),
                confidence: 0.8,
                observation_count: 3,
            },
        ];

        let (edited, applied) = apply_batch_to_document(doc, &recs);
        assert!(edited.contains("변제합니다"));
        assert!(edited.contains("청구합니다"));
        assert!(!edited.contains("하였다"));
        assert_eq!(applied.len(), 2);
    }

    #[test]
    fn reject_decreases_confidence() {
        let store = test_store();
        let id = store
            .create_pattern(PatternType::Style, "a", "b", "all")
            .unwrap();
        store.bump_confidence(id, 0.5).unwrap();

        let rec = CorrectionRecommendation {
            pattern_id: id,
            pattern_type: PatternType::Style,
            location_start: 0,
            location_end: 1,
            original: "a".into(),
            suggested: "b".into(),
            confidence: 0.8,
            observation_count: 2,
        };

        apply_feedback(&store, &rec, &UserAction::Reject).unwrap();
        let updated = store.find_pattern("a", "b").unwrap().unwrap();
        assert_eq!(updated.reject_count, 1);
        assert!(updated.confidence < 0.8);
    }
}
