//! Recommender — scan documents for matching patterns and suggest corrections.

use super::store::{CorrectionPattern, CorrectionStore, PatternType};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A correction recommendation for a specific location in a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionRecommendation {
    pub pattern_id: i64,
    pub pattern_type: PatternType,
    pub location_start: usize,
    pub location_end: usize,
    pub original: String,
    pub suggested: String,
    pub confidence: f64,
    pub observation_count: i64,
}

/// Default minimum confidence threshold for activating recommendations.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.7;

/// Scan a document and produce recommendations from active patterns.
pub fn scan_and_recommend(
    store: &CorrectionStore,
    document: &str,
    doc_type: &str,
    min_confidence: Option<f64>,
) -> Result<Vec<CorrectionRecommendation>> {
    let threshold = min_confidence.unwrap_or(DEFAULT_MIN_CONFIDENCE);
    let patterns = store.active_patterns_for_scope(doc_type)?;

    let mut recommendations = Vec::new();

    for pattern in patterns.iter().filter(|p| p.confidence >= threshold) {
        find_matches(document, pattern, &mut recommendations);
    }

    // Sort: confidence DESC, then typo > style > terminology > structure
    recommendations.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| rank_pattern_type(&a.pattern_type).cmp(&rank_pattern_type(&b.pattern_type)))
    });

    Ok(recommendations)
}

fn rank_pattern_type(pt: &PatternType) -> u8 {
    match pt {
        PatternType::Typo => 0,
        PatternType::Style => 1,
        PatternType::Terminology => 2,
        PatternType::Structure => 3,
    }
}

/// Find all matches of a pattern in the document and push recommendations.
fn find_matches(
    document: &str,
    pattern: &CorrectionPattern,
    out: &mut Vec<CorrectionRecommendation>,
) {
    // For now use literal substring matching. A future version could
    // attempt regex compilation when original_regex looks like a regex.
    let needle = &pattern.original_regex;
    if needle.is_empty() {
        return;
    }

    let mut start = 0;
    while let Some(pos) = document[start..].find(needle) {
        let abs_start = start + pos;
        let abs_end = abs_start + needle.len();

        out.push(CorrectionRecommendation {
            pattern_id: pattern.id,
            pattern_type: pattern.pattern_type,
            location_start: abs_start,
            location_end: abs_end,
            original: needle.clone(),
            suggested: pattern.replacement.clone(),
            confidence: pattern.confidence,
            observation_count: pattern.observation_count,
        });

        start = abs_end;
        if start >= document.len() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_store() -> CorrectionStore {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        CorrectionStore::new(Arc::new(Mutex::new(conn)), "test-device".into())
    }

    #[test]
    fn recommends_matching_pattern() {
        let store = test_store();
        let id = store
            .create_pattern(PatternType::Style, "하였다", "합니다", "legal_brief")
            .unwrap();
        // Boost to above threshold
        store.bump_confidence(id, 0.5).unwrap();

        let doc = "피고는 변제하였다. 원고는 청구하였다.";
        let recs = scan_and_recommend(&store, doc, "legal_brief", Some(0.5)).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].original, "하였다");
        assert_eq!(recs[0].suggested, "합니다");
    }

    #[test]
    fn skips_low_confidence_patterns() {
        let store = test_store();
        store
            .create_pattern(PatternType::Style, "하였다", "합니다", "all")
            .unwrap();
        // default confidence 0.3 — below 0.7 threshold

        let doc = "하였다";
        let recs = scan_and_recommend(&store, doc, "all", None).unwrap();
        assert!(recs.is_empty());
    }

    #[test]
    fn orders_typo_before_style() {
        let store = test_store();
        let typo = store
            .create_pattern(PatternType::Typo, "됬다", "됐다", "all")
            .unwrap();
        let style = store
            .create_pattern(PatternType::Style, "하였다", "합니다", "all")
            .unwrap();
        store.bump_confidence(typo, 0.5).unwrap();
        store.bump_confidence(style, 0.5).unwrap();

        let doc = "그는 하였다 그리고 됬다";
        let recs = scan_and_recommend(&store, doc, "all", Some(0.5)).unwrap();
        // Same confidence — typo should come first
        assert_eq!(recs[0].pattern_type, PatternType::Typo);
    }
}
