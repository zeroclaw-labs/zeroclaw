// @Ref: SUMMARY §3 Step 3 — cross-validation + size-based cap.

use super::ai_stub::KeyConcept;
use std::collections::HashMap;

/// Size-based cap (§3.2).
fn size_cap(char_count: usize) -> usize {
    match char_count {
        n if n <= 500 => 5,
        n if n <= 2_000 => 10,
        n if n <= 5_000 => 15,
        _ => 20,
    }
}

/// Merge TF scores (axis 1) with AI importance concepts (axis 2).
///
/// - Group A (present in BOTH): auto-confirmed.
/// - Group B (one-sided): kept if AI importance ≥ 7 OR TF score ≥ 3.0.
/// - Group C (neither above threshold): dropped.
/// - Final list is truncated to size-based cap.
pub fn merge(
    tf_scores: &HashMap<String, f32>,
    ai_concepts: &[KeyConcept],
    char_count: usize,
) -> Vec<String> {
    let ai_by_term: HashMap<&str, u8> =
        ai_concepts.iter().map(|c| (c.term.as_str(), c.importance)).collect();

    let mut scored: Vec<(String, f32)> = Vec::new();

    // Group A: both axes agree.
    for (term, tf) in tf_scores {
        if let Some(imp) = ai_by_term.get(term.as_str()) {
            let combined = tf * 0.5 + f32::from(*imp) * 0.5;
            scored.push((term.clone(), combined));
        }
    }
    let agreed: std::collections::HashSet<String> =
        scored.iter().map(|(k, _)| k.clone()).collect();

    // Group B: one-sided with threshold.
    for (term, tf) in tf_scores {
        if agreed.contains(term) {
            continue;
        }
        if *tf >= 3.0 {
            scored.push((term.clone(), *tf));
        }
    }
    for c in ai_concepts {
        if agreed.contains(&c.term) {
            continue;
        }
        if c.importance >= 7 {
            scored.push((c.term.clone(), f32::from(c.importance)));
        }
    }

    // Sort descending, apply cap.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
    });
    let cap = size_cap(char_count);
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(cap);
    for (term, _) in scored {
        if seen.insert(term.clone()) {
            result.push(term);
            if result.len() >= cap {
                break;
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_cap_thresholds() {
        assert_eq!(size_cap(100), 5);
        assert_eq!(size_cap(1500), 10);
        assert_eq!(size_cap(4000), 15);
        assert_eq!(size_cap(50_000), 20);
    }

    #[test]
    fn both_axes_agreement_wins() {
        let mut tf = HashMap::new();
        tf.insert("민법 제750조".to_string(), 5.0);
        let ai = vec![KeyConcept {
            term: "민법 제750조".into(),
            importance: 9,
        }];
        let result = merge(&tf, &ai, 1000);
        assert!(result.contains(&"민법 제750조".to_string()));
    }

    #[test]
    fn low_tf_only_dropped() {
        let mut tf = HashMap::new();
        tf.insert("weak".to_string(), 1.5);
        let ai = vec![];
        let result = merge(&tf, &ai, 1000);
        assert!(!result.contains(&"weak".to_string()));
    }

    #[test]
    fn high_ai_only_kept() {
        let tf = HashMap::new();
        let ai = vec![KeyConcept {
            term: "중요개념".into(),
            importance: 8,
        }];
        let result = merge(&tf, &ai, 1000);
        assert!(result.contains(&"중요개념".to_string()));
    }

    #[test]
    fn cap_respected() {
        let mut tf = HashMap::new();
        let ai: Vec<KeyConcept> = (0..30)
            .map(|i| KeyConcept {
                term: format!("term{i}"),
                importance: 9,
            })
            .collect();
        for i in 0..30 {
            tf.insert(format!("term{i}"), 10.0);
        }
        let result = merge(&tf, &ai, 100); // cap=5
        assert_eq!(result.len(), 5);
    }
}
