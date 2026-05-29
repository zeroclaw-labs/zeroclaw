use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HebbianEdge {
    pub source: String,
    pub target: String,
    pub strength: f64,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HebbianGraph {
    pub edges: HashMap<(String, String), f64>,
}

impl HebbianGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn strengthen(&mut self, a: &str, b: &str, delta: f64) {
        let key = (a.to_string(), b.to_string());
        let strength = self.edges.entry(key).or_insert(0.0);
        *strength = (*strength + delta).clamp(0.0, 1.0);
    }

    pub fn weaken(&mut self, a: &str, b: &str, delta: f64) {
        let key = (a.to_string(), b.to_string());
        if let Some(strength) = self.edges.get_mut(&key) {
            *strength = (*strength - delta).max(0.0);
        }
    }

    pub fn query_associated(&self, concept: &str, threshold: f64) -> Vec<HebbianEdge> {
        self.edges
            .iter()
            .filter(|((s, _), w)| s == concept && **w >= threshold)
            .map(|((s, t), w)| HebbianEdge {
                source: s.clone(),
                target: t.clone(),
                strength: *w,
                source_id: String::new(),
                timestamp: 0,
                confidence: *w,
            })
            .collect()
    }

    pub fn decay_all(&mut self, factor: f64) {
        for strength in self.edges.values_mut() {
            *strength *= factor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hebbian_learning_rule() {
        let mut g = HebbianGraph::new();
        g.strengthen("rust", "safety", 0.3);
        g.strengthen("rust", "safety", 0.3);
        let assoc = g.query_associated("rust", 0.5);
        assert_eq!(assoc.len(), 1);
        assert!((assoc[0].strength - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn weaken_reduces_strength() {
        let mut g = HebbianGraph::new();
        g.strengthen("a", "b", 0.8);
        g.weaken("a", "b", 0.5);
        let assoc = g.query_associated("a", 0.0);
        assert!((assoc[0].strength - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn decay_all_reduces() {
        let mut g = HebbianGraph::new();
        g.strengthen("x", "y", 1.0);
        g.decay_all(0.5);
        let assoc = g.query_associated("x", 0.0);
        assert!((assoc[0].strength - 0.5).abs() < f64::EPSILON);
    }
}
