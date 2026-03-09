use super::traits::{MemoryCategory, MemoryEntry};
use chrono::{DateTime, Utc};

/// Default half-life in days for time-decay scoring.
/// After this many days, a non-Core memory's score drops to 50%.
const DEFAULT_HALF_LIFE_DAYS: f64 = 7.0;

/// Apply exponential time decay to memory entry scores.
///
/// - `Core` memories are exempt ("evergreen") — their scores are never decayed.
/// - Entries without a parseable RFC3339 timestamp are left unchanged.
/// - Entries without a score (`None`) are left unchanged.
///
/// Decay formula: `score * 2^(-age_days / half_life_days)`
pub fn apply_time_decay(entries: &mut [MemoryEntry], half_life_days: f64) {
    let half_life = if half_life_days <= 0.0 {
        DEFAULT_HALF_LIFE_DAYS
    } else {
        half_life_days
    };

    let now = Utc::now();

    for entry in entries.iter_mut() {
        // Core memories are evergreen — never decay
        if entry.category == MemoryCategory::Core {
            continue;
        }

        let score = match entry.score {
            Some(s) => s,
            None => continue,
        };

        let ts = match DateTime::parse_from_rfc3339(&entry.timestamp) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => continue,
        };

        let age_days = now.signed_duration_since(ts).num_seconds().max(0) as f64 / 86_400.0;

        let decay_factor = (-age_days / half_life * std::f64::consts::LN_2).exp();
        entry.score = Some(score * decay_factor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(category: MemoryCategory, score: Option<f64>, timestamp: &str) -> MemoryEntry {
        MemoryEntry {
            id: "1".into(),
            key: "test".into(),
            content: "value".into(),
            category,
            timestamp: timestamp.into(),
            session_id: None,
            score,
        }
    }

    fn recent_rfc3339() -> String {
        Utc::now().to_rfc3339()
    }

    fn days_ago_rfc3339(days: i64) -> String {
        (Utc::now() - chrono::Duration::days(days)).to_rfc3339()
    }

    #[test]
    fn core_memories_are_never_decayed() {
        let mut entries = vec![make_entry(
            MemoryCategory::Core,
            Some(0.9),
            &days_ago_rfc3339(30),
        )];
        apply_time_decay(&mut entries, 7.0);
        assert_eq!(entries[0].score, Some(0.9));
    }

    #[test]
    fn recent_entry_score_barely_changes() {
        let mut entries = vec![make_entry(
            MemoryCategory::Conversation,
            Some(0.8),
            &recent_rfc3339(),
        )];
        apply_time_decay(&mut entries, 7.0);
        let decayed = entries[0].score.unwrap();
        assert!(
            (decayed - 0.8).abs() < 0.01,
            "recent entry should barely decay, got {decayed}"
        );
    }

    #[test]
    fn one_half_life_halves_score() {
        let mut entries = vec![make_entry(
            MemoryCategory::Conversation,
            Some(1.0),
            &days_ago_rfc3339(7),
        )];
        apply_time_decay(&mut entries, 7.0);
        let decayed = entries[0].score.unwrap();
        assert!(
            (decayed - 0.5).abs() < 0.05,
            "score after one half-life should be ~0.5, got {decayed}"
        );
    }

    #[test]
    fn two_half_lives_quarters_score() {
        let mut entries = vec![make_entry(
            MemoryCategory::Conversation,
            Some(1.0),
            &days_ago_rfc3339(14),
        )];
        apply_time_decay(&mut entries, 7.0);
        let decayed = entries[0].score.unwrap();
        assert!(
            (decayed - 0.25).abs() < 0.05,
            "score after two half-lives should be ~0.25, got {decayed}"
        );
    }

    #[test]
    fn no_score_entry_is_unchanged() {
        let mut entries = vec![make_entry(
            MemoryCategory::Conversation,
            None,
            &days_ago_rfc3339(30),
        )];
        apply_time_decay(&mut entries, 7.0);
        assert_eq!(entries[0].score, None);
    }

    #[test]
    fn unparseable_timestamp_is_unchanged() {
        let mut entries = vec![make_entry(
            MemoryCategory::Conversation,
            Some(0.9),
            "not-a-date",
        )];
        apply_time_decay(&mut entries, 7.0);
        assert_eq!(entries[0].score, Some(0.9));
    }
}
