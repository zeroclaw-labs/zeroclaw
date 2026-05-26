use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Belief {
    pub key: String,
    pub value: String,
    pub confidence: f64,
    pub source: BeliefSource,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BeliefSource {
    Observation,
    Inference,
    UserStatement,
    MemoryConsolidation,
    QuantumCollapse,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BeliefsStore {
    beliefs: HashMap<String, Belief>,
}

impl BeliefsStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: String, value: String, confidence: f64, source: BeliefSource) {
        let now = Utc::now();
        let existing = self.beliefs.get(&key);
        let revision = existing.map(|b| b.revision + 1).unwrap_or(1);
        let created_at = existing.map(|b| b.created_at).unwrap_or(now);

        self.beliefs.insert(
            key.clone(),
            Belief {
                key,
                value,
                confidence: confidence.clamp(0.0, 1.0),
                source,
                created_at,
                updated_at: now,
                revision,
            },
        );
    }

    pub fn get(&self, key: &str) -> Option<&Belief> {
        self.beliefs.get(key)
    }

    pub fn remove(&mut self, key: &str) -> Option<Belief> {
        self.beliefs.remove(key)
    }

    pub fn above_confidence(&self, threshold: f64) -> Vec<&Belief> {
        self.beliefs
            .values()
            .filter(|b| b.confidence >= threshold)
            .collect()
    }

    pub fn all(&self) -> impl Iterator<Item = &Belief> {
        self.beliefs.values()
    }

    pub fn len(&self) -> usize {
        self.beliefs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.beliefs.is_empty()
    }

    pub fn decay(&mut self, factor: f64) {
        let factor = factor.clamp(0.0, 1.0);
        for belief in self.beliefs.values_mut() {
            belief.confidence *= factor;
        }
    }

    pub fn prune(&mut self, min_confidence: f64) {
        self.beliefs.retain(|_, b| b.confidence >= min_confidence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_belief() {
        let mut store = BeliefsStore::new();
        store.set(
            "sky_color".into(),
            "blue".into(),
            0.95,
            BeliefSource::Observation,
        );
        let b = store.get("sky_color").unwrap();
        assert_eq!(b.value, "blue");
        assert_eq!(b.revision, 1);
    }

    #[test]
    fn revision_increments_on_update() {
        let mut store = BeliefsStore::new();
        store.set("k".into(), "v1".into(), 0.5, BeliefSource::Inference);
        store.set("k".into(), "v2".into(), 0.8, BeliefSource::Inference);
        assert_eq!(store.get("k").unwrap().revision, 2);
        assert_eq!(store.get("k").unwrap().value, "v2");
    }

    #[test]
    fn decay_reduces_confidence() {
        let mut store = BeliefsStore::new();
        store.set("k".into(), "v".into(), 1.0, BeliefSource::Observation);
        store.decay(0.5);
        let b = store.get("k").unwrap();
        assert!((b.confidence - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn prune_removes_low_confidence() {
        let mut store = BeliefsStore::new();
        store.set("strong".into(), "v".into(), 0.9, BeliefSource::Observation);
        store.set("weak".into(), "v".into(), 0.1, BeliefSource::Inference);
        store.prune(0.5);
        assert_eq!(store.len(), 1);
        assert!(store.get("strong").is_some());
    }
}
