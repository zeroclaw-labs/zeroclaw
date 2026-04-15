// @Ref: SUMMARY §3 Step 2b — boilerplate filter.

use super::ai_stub::KeyConcept;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

/// Load boilerplate words for the given domain.
/// Empty domain string matches all (domain-neutral boilerplate).
pub fn load_set(conn: &Connection, domain: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT word FROM boilerplate_words WHERE domain = ?1 OR domain IS NULL OR domain = ''",
    )?;
    let rows = stmt.query_map([domain], |r| r.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for w in rows {
        out.insert(w?);
    }
    Ok(out)
}

pub fn filter_tf(
    scores: &HashMap<String, f32>,
    boilerplate: &HashSet<String>,
) -> HashMap<String, f32> {
    scores
        .iter()
        .filter(|(k, _)| !boilerplate.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

pub fn filter_ai(
    concepts: &[KeyConcept],
    boilerplate: &HashSet<String>,
) -> Vec<KeyConcept> {
    concepts
        .iter()
        .filter(|c| !boilerplate.contains(c.term.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_matching_words() {
        let mut tf = HashMap::new();
        tf.insert("원고".to_string(), 10.0);
        tf.insert("손해배상".to_string(), 8.0);
        let mut bp = HashSet::new();
        bp.insert("원고".to_string());
        let filtered = filter_tf(&tf, &bp);
        assert!(!filtered.contains_key("원고"));
        assert!(filtered.contains_key("손해배상"));
    }

    #[test]
    fn empty_boilerplate_passes_through() {
        let mut tf = HashMap::new();
        tf.insert("A".to_string(), 1.0);
        let filtered = filter_tf(&tf, &HashSet::new());
        assert_eq!(filtered.len(), 1);
    }
}
