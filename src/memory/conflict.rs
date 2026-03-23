//! Conflict resolution for memory entries.
//!
//! Before storing Core memories, performs a semantic similarity check against
//! existing entries. If cosine similarity exceeds a threshold but content
//! differs, the old entry is marked as superseded.

use super::traits::{Memory, MemoryCategory, MemoryEntry};

/// Check for conflicting memories and mark old ones as superseded.
///
/// Returns the list of entry IDs that were superseded.
pub async fn check_and_resolve_conflicts(
    memory: &dyn Memory,
    key: &str,
    content: &str,
    category: &MemoryCategory,
    threshold: f64,
) -> anyhow::Result<Vec<String>> {
    // Only check conflicts for Core memories
    if !matches!(category, MemoryCategory::Core) {
        return Ok(Vec::new());
    }

    // Search for similar existing entries
    let candidates = memory.recall(content, 10, None, None, None).await?;

    let mut superseded = Vec::new();
    for candidate in &candidates {
        if candidate.key == key {
            continue; // Same key = update, not conflict
        }
        if !matches!(candidate.category, MemoryCategory::Core) {
            continue;
        }
        if let Some(score) = candidate.score {
            if score > threshold && candidate.content != content {
                superseded.push(candidate.id.clone());
            }
        }
    }

    Ok(superseded)
}

/// Mark entries as superseded in SQLite by setting their `superseded_by` column.
pub fn mark_superseded(
    conn: &rusqlite::Connection,
    superseded_ids: &[String],
    new_id: &str,
) -> anyhow::Result<()> {
    if superseded_ids.is_empty() {
        return Ok(());
    }

    for id in superseded_ids {
        conn.execute(
            "UPDATE memories SET superseded_by = ?1 WHERE id = ?2",
            rusqlite::params![new_id, id],
        )?;
    }

    Ok(())
}

/// Simple text-based conflict detection without embeddings.
///
/// Uses token overlap (Jaccard similarity) as a fast approximation
/// when vector embeddings are unavailable.
pub fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }
    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Find potentially conflicting entries using text similarity when embeddings
/// are not available. Returns entries above the threshold.
pub fn find_text_conflicts(
    entries: &[MemoryEntry],
    new_content: &str,
    threshold: f64,
) -> Vec<String> {
    entries
        .iter()
        .filter(|e| {
            matches!(e.category, MemoryCategory::Core)
                && e.superseded_by.is_none()
                && jaccard_similarity(&e.content, new_content) > threshold
                && e.content != new_content
        })
        .map(|e| e.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_identical_strings() {
        let sim = jaccard_similarity("hello world", "hello world");
        assert!((sim - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint_strings() {
        let sim = jaccard_similarity("hello world", "foo bar");
        assert!(sim.abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let sim = jaccard_similarity("the quick brown fox", "the slow brown dog");
        // overlap: "the", "brown" = 2; union: "the", "quick", "brown", "fox", "slow", "dog" = 6
        assert!((sim - 2.0 / 6.0).abs() < 0.01);
    }

    #[test]
    fn jaccard_empty_strings() {
        assert!((jaccard_similarity("", "") - 1.0).abs() < f64::EPSILON);
        assert!(jaccard_similarity("hello", "").abs() < f64::EPSILON);
        assert!(jaccard_similarity("", "hello").abs() < f64::EPSILON);
    }

    #[test]
    fn find_text_conflicts_filters_correctly() {
        let entries = vec![
            MemoryEntry {
                id: "1".into(),
                key: "pref".into(),
                content: "User prefers Rust for systems work".into(),
                category: MemoryCategory::Core,
                timestamp: "now".into(),
                session_id: None,
                score: None,
                namespace: "default".into(),
                importance: Some(0.7),
                superseded_by: None,
                metadata: None,
            },
            MemoryEntry {
                id: "2".into(),
                key: "daily1".into(),
                content: "User prefers Rust for systems work".into(),
                category: MemoryCategory::Daily,
                timestamp: "now".into(),
                session_id: None,
                score: None,
                namespace: "default".into(),
                importance: Some(0.3),
                superseded_by: None,
                metadata: None,
            },
        ];

        // Only Core entries should be flagged
        let conflicts = find_text_conflicts(&entries, "User now prefers Go for systems work", 0.3);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0], "1");
    }
}
