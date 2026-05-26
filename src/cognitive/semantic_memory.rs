use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticFact {
    pub key: String,
    pub value: String,
    pub domain: String,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticMemory {
    pub facts: HashMap<String, SemanticFact>,
}

impl SemanticMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(
        &mut self,
        key: &str,
        value: &str,
        domain: &str,
        source_id: &str,
        timestamp: u64,
        confidence: f64,
    ) {
        self.facts.insert(
            key.to_string(),
            SemanticFact {
                key: key.to_string(),
                value: value.to_string(),
                domain: domain.to_string(),
                source_id: source_id.to_string(),
                timestamp,
                confidence: confidence.clamp(0.0, 1.0),
            },
        );
    }

    pub fn retrieve(&self, key: &str) -> Option<&SemanticFact> {
        self.facts.get(key)
    }

    pub fn query_by_domain(&self, domain: &str) -> Vec<&SemanticFact> {
        self.facts.values().filter(|f| f.domain == domain).collect()
    }

    pub fn relevance_score(&self, query: &str) -> Vec<(&SemanticFact, f64)> {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();
        let mut scored: Vec<(&SemanticFact, f64)> = self
            .facts
            .values()
            .filter_map(|fact| {
                let fact_text =
                    format!("{} {} {}", fact.key, fact.value, fact.domain).to_lowercase();
                let matches = query_words
                    .iter()
                    .filter(|w| fact_text.contains(**w))
                    .count();
                if matches > 0 {
                    let raw_score = matches as f64 / query_words.len().max(1) as f64;
                    let weighted = raw_score * fact.confidence;
                    Some((fact, weighted))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scored
    }

    pub fn high_confidence(&self, threshold: f64) -> Vec<&SemanticFact> {
        self.facts
            .values()
            .filter(|f| f.confidence >= threshold)
            .collect()
    }

    pub fn forget(&mut self, key: &str) -> Option<SemanticFact> {
        self.facts.remove(key)
    }

    pub fn decay_confidence(&mut self, factor: f64) {
        for fact in self.facts.values_mut() {
            fact.confidence *= factor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve() {
        let mut sm = SemanticMemory::new();
        sm.store(
            "rust_type",
            "systems language",
            "programming",
            "observer",
            1,
            0.9,
        );
        let fact = sm.retrieve("rust_type").unwrap();
        assert_eq!(fact.value, "systems language");
        assert_eq!(fact.domain, "programming");
        assert_eq!(fact.source_id, "observer");
        assert_eq!(fact.timestamp, 1);
        assert_eq!(fact.confidence, 0.9);
    }

    #[test]
    fn query_by_domain_filters() {
        let mut sm = SemanticMemory::new();
        sm.store("rust", "lang", "programming", "src", 1, 0.8);
        sm.store("tcp", "protocol", "networking", "src", 2, 0.7);
        sm.store("python", "lang", "programming", "src", 3, 0.6);
        let prog = sm.query_by_domain("programming");
        assert_eq!(prog.len(), 2);
        let net = sm.query_by_domain("networking");
        assert_eq!(net.len(), 1);
    }

    #[test]
    fn relevance_score_ranks() {
        let mut sm = SemanticMemory::new();
        sm.store(
            "rust_safety",
            "memory safe language",
            "programming",
            "src",
            1,
            0.9,
        );
        sm.store("python_easy", "easy to learn", "programming", "src", 2, 0.8);
        sm.store("tcp_stack", "network protocol", "networking", "src", 3, 0.7);
        let results = sm.relevance_score("rust memory safe");
        assert!(!results.is_empty());
        assert_eq!(results[0].0.key, "rust_safety");
    }

    #[test]
    fn high_confidence_filters() {
        let mut sm = SemanticMemory::new();
        sm.store("a", "v", "d", "s", 1, 0.9);
        sm.store("b", "v", "d", "s", 2, 0.3);
        assert_eq!(sm.high_confidence(0.5).len(), 1);
    }

    #[test]
    fn forget_removes_fact() {
        let mut sm = SemanticMemory::new();
        sm.store("temp", "value", "domain", "src", 1, 0.5);
        assert!(sm.forget("temp").is_some());
        assert!(sm.retrieve("temp").is_none());
    }

    #[test]
    fn decay_reduces_confidence() {
        let mut sm = SemanticMemory::new();
        sm.store("fact", "val", "dom", "src", 1, 1.0);
        sm.decay_confidence(0.5);
        let fact = sm.retrieve("fact").unwrap();
        assert!((fact.confidence - 0.5).abs() < f64::EPSILON);
    }
}
