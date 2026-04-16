//! Pattern miner — aggregate correction observations into reusable patterns.
//!
//! Observations arrive as (original → corrected) pairs. The miner checks
//! whether the pair already matches an existing pattern (boosts confidence)
//! or creates a new pattern (initial confidence 0.3).
//!
//! Deeper generalization (e.g., "하였다 → 합니다" across many verb stems) is
//! delegated to the LLM via `GENERALIZE_PROMPT`.

use super::observer::CorrectionObservation;
use super::store::{CorrectionStore, PatternType};
use anyhow::Result;

/// A pattern update produced by the miner.
#[derive(Debug, Clone)]
pub struct PatternUpdate {
    pub pattern_id: i64,
    pub is_new: bool,
    pub observation_count: i64,
    pub confidence: f64,
}

/// Confidence delta applied when a new observation matches an existing pattern.
const CONFIDENCE_BOOST: f64 = 0.25;
/// Minimum observations before a pattern is promoted to "active"
/// (i.e., eligible for recommendations).
pub const PROMOTION_THRESHOLD: i64 = 3;
/// Confidence at which recommendations activate.
pub const RECOMMENDATION_THRESHOLD: f64 = 0.7;

/// System prompt for LLM-assisted pattern generalization.
pub const GENERALIZE_PROMPT: &str = r#"The user has made repeated similar text edits.
Examples:
{examples}

Identify the general pattern and express it as:
- original_regex: A regex or literal string that captures the pattern
- replacement: The replacement (may use $1, $2 for regex groups)
- pattern_type: typo | style | terminology | structure
- scope: all | <document_type>

Respond in JSON format. Only generalize when the pattern is clear."#;

/// Mine a single observation into a pattern update.
///
/// If a matching pattern exists, boost its confidence. Otherwise, create
/// a new pattern at initial confidence 0.3.
pub fn mine_observation(
    store: &CorrectionStore,
    obs: &CorrectionObservation,
) -> Result<PatternUpdate> {
    // Direct literal match — easy case
    if let Some(existing) = store.find_pattern(&obs.original_text, &obs.corrected_text)? {
        store.bump_confidence(existing.id, CONFIDENCE_BOOST)?;
        if let Some(obs_id) = obs.id {
            store.link_observation(existing.id, obs_id)?;
        }
        // Reload to get updated counts
        let updated = store
            .find_pattern(&obs.original_text, &obs.corrected_text)?
            .expect("pattern should exist after bump");
        return Ok(PatternUpdate {
            pattern_id: updated.id,
            is_new: false,
            observation_count: updated.observation_count,
            confidence: updated.confidence,
        });
    }

    // New pattern — classify into type using simple heuristics
    let pattern_type = classify_pattern_type(&obs.original_text, &obs.corrected_text);
    let scope = obs.document_type.as_deref().unwrap_or("all");

    let pattern_id = store.create_pattern(
        pattern_type,
        &obs.original_text,
        &obs.corrected_text,
        scope,
    )?;

    if let Some(obs_id) = obs.id {
        store.link_observation(pattern_id, obs_id)?;
    }

    Ok(PatternUpdate {
        pattern_id,
        is_new: true,
        observation_count: 1,
        confidence: 0.3,
    })
}

/// Mine a batch of observations.
pub fn mine_patterns(
    store: &CorrectionStore,
    observations: &[CorrectionObservation],
) -> Result<Vec<PatternUpdate>> {
    observations.iter().map(|o| mine_observation(store, o)).collect()
}

/// Heuristic classification of a correction into a pattern type.
fn classify_pattern_type(original: &str, corrected: &str) -> PatternType {
    let orig_chars: Vec<char> = original.chars().collect();
    let corr_chars: Vec<char> = corrected.chars().collect();

    let len_diff = orig_chars.len().abs_diff(corr_chars.len());
    let min_len = orig_chars.len().min(corr_chars.len()).max(1);

    // Tight typo: same length (or off-by-one) and edit distance ≤ 1 means
    // a single character substitution — classic misspelling fix.
    if len_diff <= 1 && min_len >= 2 {
        let edit_dist = levenshtein_capped(original, corrected, 2);
        if edit_dist <= 1 {
            return PatternType::Typo;
        }
    }

    // Style change: equal-length strings that share prefix OR trailing
    // characters (e.g. Korean ending conjugation 하였다 → 합니다 shares 다).
    if orig_chars.len() == corr_chars.len() && ends_differ_only(original, corrected) {
        return PatternType::Style;
    }

    // Large semantic replacement — terminology
    if orig_chars.len() >= 2 && corr_chars.len() >= 2 {
        return PatternType::Terminology;
    }

    PatternType::Structure
}

/// Check if two strings share a structural boundary (common prefix OR
/// common trailing character) that implies a style/conjugation change
/// rather than a scattered rewrite.
fn ends_differ_only(a: &str, b: &str) -> bool {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let min_len = a_chars.len().min(b_chars.len());

    if min_len < 2 {
        return false;
    }

    // Common prefix
    let prefix = a_chars
        .iter()
        .zip(b_chars.iter())
        .take_while(|(x, y)| x == y)
        .count();

    // Common suffix (trailing character match)
    let suffix = a_chars
        .iter()
        .rev()
        .zip(b_chars.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();

    // Half-length threshold on either side suggests a structural/ending
    // change rather than a scattered rewrite.
    let half = min_len / 2;
    prefix >= half.max(1) || suffix >= 1
}

/// Capped Levenshtein distance — returns `cap` if distance exceeds it.
fn levenshtein_capped(a: &str, b: &str, cap: usize) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();

    if n.abs_diff(m) > cap {
        return cap;
    }

    let mut prev = (0..=m).collect::<Vec<_>>();
    let mut curr = vec![0usize; m + 1];

    for i in 1..=n {
        curr[0] = i;
        let mut row_min = usize::MAX;
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            curr[j] = (curr[j - 1] + 1)
                .min(prev[j] + 1)
                .min(prev[j - 1] + cost);
            row_min = row_min.min(curr[j]);
        }
        if row_min > cap {
            return cap;
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[m].min(cap)
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

    fn obs(before: &str, after: &str) -> CorrectionObservation {
        CorrectionObservation {
            id: Some(1),
            uuid: uuid::Uuid::new_v4().to_string(),
            original_text: before.into(),
            corrected_text: after.into(),
            context_before: None,
            context_after: None,
            document_type: Some("legal_brief".into()),
            category: Some("document".into()),
            source: "user_edit".into(),
            grammar_valid: true,
            observed_at: 0,
            session_id: None,
        }
    }

    #[test]
    fn mines_new_pattern_first_time() {
        let store = test_store();
        let o = obs("하였다", "합니다");
        let update = mine_observation(&store, &o).unwrap();
        assert!(update.is_new);
        assert!((update.confidence - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn mines_existing_pattern_boosts_confidence() {
        let store = test_store();
        let o = obs("하였다", "합니다");
        mine_observation(&store, &o).unwrap();
        let update = mine_observation(&store, &o).unwrap();
        assert!(!update.is_new);
        assert!(update.confidence > 0.3);
        assert_eq!(update.observation_count, 2);
    }

    #[test]
    fn three_observations_reach_promotion() {
        let store = test_store();
        let o = obs("하였다", "합니다");
        for _ in 0..3 {
            mine_observation(&store, &o).unwrap();
        }
        let pattern = store.find_pattern("하였다", "합니다").unwrap().unwrap();
        // 0.3 + 0.25 + 0.25 = 0.8
        assert!(pattern.confidence >= 0.7);
        assert_eq!(pattern.observation_count, 3);
    }

    #[test]
    fn classify_typo() {
        assert_eq!(classify_pattern_type("됬다", "됐다"), PatternType::Typo);
    }

    #[test]
    fn classify_style() {
        assert_eq!(classify_pattern_type("하였다", "합니다"), PatternType::Style);
    }

    #[test]
    fn levenshtein_cap_works() {
        assert_eq!(levenshtein_capped("abc", "abc", 3), 0);
        assert_eq!(levenshtein_capped("abc", "xyz", 3), 3);
        assert_eq!(levenshtein_capped("abc", "abcdef", 3), 3);
    }
}
