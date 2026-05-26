use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BeliefSource {
    Observed,
    Predicted,
    Corrected,
    Assumed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfBelief {
    pub key: String,
    pub value: f64,
    pub confidence: f32,
    pub source: BeliefSource,
    pub revision: u32,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldBelief {
    pub key: String,
    pub value: f64,
    pub confidence: f32,
    pub source: BeliefSource,
    pub revision: u32,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfModel {
    beliefs: HashMap<String, SelfBelief>,
    capacity: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldModel {
    beliefs: HashMap<String, WorldBelief>,
    capacity: usize,
}

impl SelfModel {
    pub fn new(capacity: usize) -> Self {
        Self {
            beliefs: HashMap::new(),
            capacity,
        }
    }

    pub fn update_belief(&mut self, key: &str, value: f64, confidence: f32, source: BeliefSource) {
        let entry = self
            .beliefs
            .entry(key.to_string())
            .or_insert_with(|| SelfBelief {
                key: key.to_string(),
                value: 0.0,
                confidence: 0.0,
                source: BeliefSource::Assumed,
                revision: 0,
                last_updated: Utc::now(),
            });

        entry.value = value;
        entry.confidence = confidence.clamp(0.0, 1.0);
        entry.source = source;
        entry.revision += 1;
        entry.last_updated = Utc::now();

        if self.beliefs.len() > self.capacity {
            self.prune_lowest_confidence();
        }
    }

    pub fn get_belief(&self, key: &str) -> Option<&SelfBelief> {
        self.beliefs.get(key)
    }

    pub fn confidence(&self, key: &str) -> f32 {
        self.beliefs.get(key).map_or(0.0, |b| b.confidence)
    }

    pub fn metacognitive_accuracy(&self) -> f64 {
        if self.beliefs.is_empty() {
            return 0.0;
        }
        let observed: Vec<&SelfBelief> = self
            .beliefs
            .values()
            .filter(|b| b.source == BeliefSource::Observed || b.source == BeliefSource::Corrected)
            .collect();
        if observed.is_empty() {
            return 0.5;
        }
        let avg_confidence: f64 = observed
            .iter()
            .map(|b| f64::from(b.confidence))
            .sum::<f64>()
            / observed.len() as f64;
        avg_confidence
    }

    pub fn divergence_from(&self, world: &WorldModel) -> f64 {
        let mut total_divergence = 0.0;
        let mut count = 0usize;

        for (key, self_belief) in &self.beliefs {
            if let Some(world_belief) = world.get_belief(key) {
                let delta = (self_belief.value - world_belief.value).abs();
                let confidence_weight = f64::midpoint(
                    f64::from(self_belief.confidence),
                    f64::from(world_belief.confidence),
                );
                total_divergence += delta * confidence_weight;
                count += 1;
            }
        }

        if count == 0 {
            return 0.0;
        }
        total_divergence / count as f64
    }

    pub fn stale_beliefs(&self, max_age_secs: i64) -> Vec<&SelfBelief> {
        let cutoff = Utc::now() - chrono::Duration::seconds(max_age_secs);
        self.beliefs
            .values()
            .filter(|b| b.last_updated < cutoff)
            .collect()
    }

    pub fn most_uncertain(&self, top_n: usize) -> Vec<&SelfBelief> {
        let mut sorted: Vec<&SelfBelief> = self.beliefs.values().collect();
        sorted.sort_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted.truncate(top_n);
        sorted
    }

    pub fn belief_count(&self) -> usize {
        self.beliefs.len()
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    pub fn restore(value: &serde_json::Value, capacity: usize) -> Option<Self> {
        let mut model: Self = serde_json::from_value(value.clone()).ok()?;
        model.capacity = capacity;
        Some(model)
    }

    fn prune_lowest_confidence(&mut self) {
        if let Some(weakest) = self
            .beliefs
            .values()
            .min_by(|a, b| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|b| b.key.clone())
        {
            self.beliefs.remove(&weakest);
        }
    }
}

impl WorldModel {
    pub fn new(capacity: usize) -> Self {
        Self {
            beliefs: HashMap::new(),
            capacity,
        }
    }

    pub fn update_belief(&mut self, key: &str, value: f64, confidence: f32, source: BeliefSource) {
        let entry = self
            .beliefs
            .entry(key.to_string())
            .or_insert_with(|| WorldBelief {
                key: key.to_string(),
                value: 0.0,
                confidence: 0.0,
                source: BeliefSource::Assumed,
                revision: 0,
                last_updated: Utc::now(),
            });

        entry.value = value;
        entry.confidence = confidence.clamp(0.0, 1.0);
        entry.source = source;
        entry.revision += 1;
        entry.last_updated = Utc::now();

        if self.beliefs.len() > self.capacity {
            self.prune_lowest_confidence();
        }
    }

    pub fn get_belief(&self, key: &str) -> Option<&WorldBelief> {
        self.beliefs.get(key)
    }

    pub fn confidence(&self, key: &str) -> f32 {
        self.beliefs.get(key).map_or(0.0, |b| b.confidence)
    }

    pub fn surprise_if_observed(&self, key: &str, observed_value: f64) -> f64 {
        let belief = match self.beliefs.get(key) {
            Some(b) => b,
            None => return 1.0,
        };
        let error = (observed_value - belief.value).abs().min(0.99);
        let surprise = -(1.0 - error).log2();
        surprise.clamp(0.0, 10.0)
    }

    pub fn most_confident(&self, top_n: usize) -> Vec<&WorldBelief> {
        let mut sorted: Vec<&WorldBelief> = self.beliefs.values().collect();
        sorted.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted.truncate(top_n);
        sorted
    }

    pub fn prediction_accuracy(&self, key: &str, observed: f64) -> f64 {
        match self.beliefs.get(key) {
            Some(b) => 1.0 - (b.value - observed).abs().min(1.0),
            None => 0.0,
        }
    }

    pub fn belief_count(&self) -> usize {
        self.beliefs.len()
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    pub fn restore(value: &serde_json::Value, capacity: usize) -> Option<Self> {
        let mut model: Self = serde_json::from_value(value.clone()).ok()?;
        model.capacity = capacity;
        Some(model)
    }

    fn prune_lowest_confidence(&mut self) {
        if let Some(weakest) = self
            .beliefs
            .values()
            .min_by(|a, b| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|b| b.key.clone())
        {
            self.beliefs.remove(&weakest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_model_update_and_retrieve() {
        let mut sm = SelfModel::new(100);
        sm.update_belief("skill_level", 0.7, 0.9, BeliefSource::Observed);
        let b = sm.get_belief("skill_level").unwrap();
        assert!((b.value - 0.7).abs() < f64::EPSILON);
        assert!((b.confidence - 0.9).abs() < f32::EPSILON);
        assert_eq!(b.revision, 1);
    }

    #[test]
    fn self_model_revision_increments() {
        let mut sm = SelfModel::new(100);
        sm.update_belief("x", 0.5, 0.5, BeliefSource::Assumed);
        sm.update_belief("x", 0.8, 0.9, BeliefSource::Corrected);
        assert_eq!(sm.get_belief("x").unwrap().revision, 2);
    }

    #[test]
    fn self_model_capacity_prunes() {
        let mut sm = SelfModel::new(2);
        sm.update_belief("a", 0.5, 0.9, BeliefSource::Observed);
        sm.update_belief("b", 0.5, 0.1, BeliefSource::Assumed);
        sm.update_belief("c", 0.5, 0.8, BeliefSource::Observed);
        assert!(sm.belief_count() <= 2);
        assert!(
            sm.get_belief("b").is_none(),
            "lowest confidence should be pruned"
        );
    }

    #[test]
    fn world_model_surprise_unknown_key() {
        let wm = WorldModel::new(100);
        let s = wm.surprise_if_observed("unknown", 0.5);
        assert!((s - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn world_model_surprise_accurate_prediction() {
        let mut wm = WorldModel::new(100);
        wm.update_belief("temp", 0.5, 0.9, BeliefSource::Predicted);
        let s = wm.surprise_if_observed("temp", 0.51);
        assert!(s < 0.5, "accurate prediction should have low surprise: {s}");
    }

    #[test]
    fn world_model_surprise_wrong_prediction() {
        let mut wm = WorldModel::new(100);
        wm.update_belief("temp", 0.1, 0.9, BeliefSource::Predicted);
        let s = wm.surprise_if_observed("temp", 0.95);
        assert!(s > 1.0, "wrong prediction should have high surprise: {s}");
    }

    #[test]
    fn divergence_zero_when_aligned() {
        let mut sm = SelfModel::new(100);
        let mut wm = WorldModel::new(100);
        sm.update_belief("x", 0.5, 0.9, BeliefSource::Observed);
        wm.update_belief("x", 0.5, 0.9, BeliefSource::Observed);
        let d = sm.divergence_from(&wm);
        assert!(
            d < f64::EPSILON,
            "aligned models should have zero divergence: {d}"
        );
    }

    #[test]
    fn divergence_positive_when_misaligned() {
        let mut sm = SelfModel::new(100);
        let mut wm = WorldModel::new(100);
        sm.update_belief("x", 0.1, 0.9, BeliefSource::Observed);
        wm.update_belief("x", 0.9, 0.9, BeliefSource::Observed);
        let d = sm.divergence_from(&wm);
        assert!(d > 0.5, "misaligned models should diverge: {d}");
    }

    #[test]
    fn metacognitive_accuracy_reflects_confidence() {
        let mut sm = SelfModel::new(100);
        sm.update_belief("a", 0.5, 0.95, BeliefSource::Observed);
        sm.update_belief("b", 0.5, 0.85, BeliefSource::Observed);
        let acc = sm.metacognitive_accuracy();
        assert!(acc > 0.8, "high-confidence observed beliefs: {acc}");
    }

    #[test]
    fn most_uncertain_returns_lowest_confidence() {
        let mut sm = SelfModel::new(100);
        sm.update_belief("sure", 0.5, 0.99, BeliefSource::Observed);
        sm.update_belief("unsure", 0.5, 0.1, BeliefSource::Assumed);
        sm.update_belief("medium", 0.5, 0.5, BeliefSource::Predicted);
        let uncertain = sm.most_uncertain(2);
        assert_eq!(uncertain[0].key, "unsure");
    }

    #[test]
    fn prediction_accuracy_perfect() {
        let mut wm = WorldModel::new(100);
        wm.update_belief("val", 0.7, 0.9, BeliefSource::Predicted);
        let acc = wm.prediction_accuracy("val", 0.7);
        assert!((acc - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn prediction_accuracy_miss() {
        let mut wm = WorldModel::new(100);
        wm.update_belief("val", 0.1, 0.9, BeliefSource::Predicted);
        let acc = wm.prediction_accuracy("val", 0.9);
        assert!(acc < 0.5, "big miss should have low accuracy: {acc}");
    }

    #[test]
    fn empty_models_safe_defaults() {
        let sm = SelfModel::new(100);
        let wm = WorldModel::new(100);
        assert_eq!(sm.belief_count(), 0);
        assert_eq!(wm.belief_count(), 0);
        assert!((sm.metacognitive_accuracy() - 0.0).abs() < f64::EPSILON);
        assert!((sm.divergence_from(&wm) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn confidence_clamped() {
        let mut sm = SelfModel::new(100);
        sm.update_belief("x", 0.5, 1.5, BeliefSource::Observed);
        assert!((sm.get_belief("x").unwrap().confidence - 1.0).abs() < f32::EPSILON);
        sm.update_belief("y", 0.5, -0.5, BeliefSource::Observed);
        assert!((sm.get_belief("y").unwrap().confidence - 0.0).abs() < f32::EPSILON);
    }
}
