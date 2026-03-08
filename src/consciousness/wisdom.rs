use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WisdomEntry {
    pub principle: String,
    pub evidence_count: u32,
    pub confidence: f64,
    pub domain: String,
}

pub struct WisdomAccumulator {
    entries: Vec<WisdomEntry>,
    capacity: usize,
}

impl WisdomAccumulator {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity,
        }
    }

    pub fn extract_wisdom(&mut self, dream_patterns: &[String]) {
        for pattern in dream_patterns {
            if let Some(existing) = self.entries.iter_mut().find(|e| e.principle == *pattern) {
                existing.evidence_count += 1;
                existing.confidence = (existing.confidence + 0.1).min(1.0);
            } else {
                if self.entries.len() >= self.capacity {
                    if let Some(min_idx) = self
                        .entries
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| {
                            a.confidence
                                .partial_cmp(&b.confidence)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .map(|(i, _)| i)
                    {
                        self.entries.remove(min_idx);
                    }
                }
                self.entries.push(WisdomEntry {
                    principle: pattern.clone(),
                    evidence_count: 1,
                    confidence: 0.3,
                    domain: Self::classify_domain(pattern),
                });
            }
        }
    }

    fn classify_domain(pattern: &str) -> String {
        if pattern.contains("coherence") {
            "stability".to_string()
        } else if pattern.contains("valence") {
            "affect".to_string()
        } else if pattern.contains("failure") {
            "resilience".to_string()
        } else {
            "general".to_string()
        }
    }

    pub fn entries(&self) -> &[WisdomEntry] {
        &self.entries
    }

    pub fn high_confidence_wisdom(&self) -> Vec<&WisdomEntry> {
        self.entries
            .iter()
            .filter(|e| e.confidence >= 0.5)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_wisdom_adds_new_entries() {
        let mut acc = WisdomAccumulator::new(10);
        acc.extract_wisdom(&["coherence_trend:declining".to_string()]);
        assert_eq!(acc.entries().len(), 1);
        assert_eq!(acc.entries()[0].principle, "coherence_trend:declining");
        assert_eq!(acc.entries()[0].domain, "stability");
        assert_eq!(acc.entries()[0].evidence_count, 1);
    }

    #[test]
    fn extract_wisdom_increments_existing() {
        let mut acc = WisdomAccumulator::new(10);
        let pattern = "valence_shift:positive".to_string();
        acc.extract_wisdom(std::slice::from_ref(&pattern));
        acc.extract_wisdom(std::slice::from_ref(&pattern));
        assert_eq!(acc.entries().len(), 1);
        assert_eq!(acc.entries()[0].evidence_count, 2);
        assert!((acc.entries()[0].confidence - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn extract_wisdom_evicts_lowest_confidence_at_capacity() {
        let mut acc = WisdomAccumulator::new(2);
        acc.extract_wisdom(&["a".to_string(), "b".to_string()]);
        assert_eq!(acc.entries().len(), 2);
        acc.extract_wisdom(&["b".to_string()]);
        acc.extract_wisdom(&["c".to_string()]);
        assert_eq!(acc.entries().len(), 2);
        assert!(acc.entries().iter().any(|e| e.principle == "b"));
        assert!(acc.entries().iter().any(|e| e.principle == "c"));
    }

    #[test]
    fn classify_domain_maps_correctly() {
        assert_eq!(
            WisdomAccumulator::classify_domain("coherence_trend"),
            "stability"
        );
        assert_eq!(
            WisdomAccumulator::classify_domain("valence_shift"),
            "affect"
        );
        assert_eq!(
            WisdomAccumulator::classify_domain("failure_mode"),
            "resilience"
        );
        assert_eq!(
            WisdomAccumulator::classify_domain("random_pattern"),
            "general"
        );
    }

    #[test]
    fn high_confidence_wisdom_filters() {
        let mut acc = WisdomAccumulator::new(10);
        acc.extract_wisdom(&["a".to_string()]);
        assert!(acc.high_confidence_wisdom().is_empty());
        for _ in 0..3 {
            acc.extract_wisdom(&["a".to_string()]);
        }
        assert!(!acc.high_confidence_wisdom().is_empty());
    }
}
