use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prediction {
    pub id: String,
    pub domain: String,
    pub predicted_value: f64,
    pub confidence: f32,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub prediction_id: String,
    pub actual_value: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionError {
    pub prediction_id: String,
    pub domain: String,
    pub error_magnitude: f64,
    pub surprise: f64,
    pub timestamp: DateTime<Utc>,
}

const EMA_ALPHA: f64 = 0.1;
const SURPRISE_CLAMP_MAX: f64 = 10.0;

fn compute_surprise(error_magnitude: f64) -> f64 {
    let clamped = error_magnitude.abs().min(0.99);
    let raw = -(1.0 - clamped).log2();
    raw.clamp(0.0, SURPRISE_CLAMP_MAX)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreeEnergyState {
    predictions: Vec<Prediction>,
    errors: Vec<PredictionError>,
    domain_accuracy: HashMap<String, f64>,
    total_free_energy: f64,
    model_updates: u64,
    capacity: usize,
}

impl FreeEnergyState {
    pub fn new(capacity: usize) -> Self {
        Self {
            predictions: Vec::with_capacity(capacity),
            errors: Vec::with_capacity(capacity),
            domain_accuracy: HashMap::new(),
            total_free_energy: 0.0,
            model_updates: 0,
            capacity,
        }
    }

    pub fn predict(&mut self, domain: &str, value: f64, confidence: f32) -> String {
        let id = format!("pred_{}_{}", domain, self.predictions.len());
        let prediction = Prediction {
            id: id.clone(),
            domain: domain.to_string(),
            predicted_value: value,
            confidence: confidence.clamp(0.0, 1.0),
            timestamp: Utc::now(),
        };
        self.predictions.push(prediction);
        if self.predictions.len() > self.capacity {
            self.predictions.remove(0);
        }
        id
    }

    pub fn observe(&mut self, prediction_id: &str, actual: f64) -> Option<PredictionError> {
        let prediction = self.predictions.iter().find(|p| p.id == prediction_id)?;
        let error_magnitude = actual - prediction.predicted_value;
        let surprise = compute_surprise(error_magnitude);
        let domain = prediction.domain.clone();

        let prediction_error = PredictionError {
            prediction_id: prediction_id.to_string(),
            domain: domain.clone(),
            error_magnitude,
            surprise,
            timestamp: Utc::now(),
        };

        self.errors.push(prediction_error.clone());
        if self.errors.len() > self.capacity {
            self.errors.remove(0);
        }

        let normalized_accuracy = 1.0 - error_magnitude.abs().min(1.0);
        let current = self.domain_accuracy.get(&domain).copied().unwrap_or(0.5);
        let updated = EMA_ALPHA * normalized_accuracy + (1.0 - EMA_ALPHA) * current;
        self.domain_accuracy.insert(domain, updated);

        self.total_free_energy = self.free_energy();
        self.model_updates += 1;

        Some(prediction_error)
    }

    pub fn free_energy(&self) -> f64 {
        if self.errors.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.errors.iter().map(|e| e.surprise).sum();
        sum / self.errors.len() as f64
    }

    pub fn domain_surprise(&self, domain: &str) -> Option<f64> {
        let domain_errors: Vec<&PredictionError> =
            self.errors.iter().filter(|e| e.domain == domain).collect();
        if domain_errors.is_empty() {
            return None;
        }
        let sum: f64 = domain_errors.iter().map(|e| e.surprise).sum();
        Some(sum / domain_errors.len() as f64)
    }

    pub fn most_surprising_domains(&self, top_n: usize) -> Vec<(String, f64)> {
        let mut domain_surprises: HashMap<String, Vec<f64>> = HashMap::new();
        for error in &self.errors {
            domain_surprises
                .entry(error.domain.clone())
                .or_default()
                .push(error.surprise);
        }
        let mut averages: Vec<(String, f64)> = domain_surprises
            .into_iter()
            .map(|(domain, surprises)| {
                let avg = surprises.iter().sum::<f64>() / surprises.len() as f64;
                (domain, avg)
            })
            .collect();
        averages.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        averages.truncate(top_n);
        averages
    }

    pub fn should_update_model(&self, domain: &str, threshold: f64) -> bool {
        self.domain_surprise(domain)
            .map_or(false, |s| s > threshold)
    }

    pub fn should_act(&self, threshold: f64) -> bool {
        self.free_energy() > threshold
    }

    pub fn accuracy(&self, domain: &str) -> Option<f64> {
        self.domain_accuracy.get(domain).copied()
    }

    pub fn reset_domain(&mut self, domain: &str) {
        self.predictions.retain(|p| p.domain != domain);
        self.errors.retain(|e| e.domain != domain);
        self.domain_accuracy.remove(domain);
        self.total_free_energy = self.free_energy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predict_then_observe_error_magnitude() {
        let mut state = FreeEnergyState::new(100);
        let id = state.predict("tool_success", 0.8, 0.9);
        let error = state.observe(&id, 0.6).unwrap();
        let expected = 0.6 - 0.8;
        assert!((error.error_magnitude - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn surprise_higher_for_larger_errors() {
        let small = compute_surprise(0.1);
        let large = compute_surprise(0.8);
        assert!(
            large > small,
            "large error={large} should exceed small={small}"
        );
    }

    #[test]
    fn surprise_clamped_to_range() {
        let s = compute_surprise(0.999);
        assert!(s <= SURPRISE_CLAMP_MAX);
        assert!(s >= 0.0);

        let s_zero = compute_surprise(0.0);
        assert!((s_zero - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn domain_accuracy_tracks_over_time() {
        let mut state = FreeEnergyState::new(100);

        for _ in 0..20 {
            let id = state.predict("user_mood", 0.5, 0.8);
            state.observe(&id, 0.52);
        }

        let acc = state.accuracy("user_mood").unwrap();
        assert!(
            acc > 0.7,
            "accuracy should converge upward for small errors: {acc}"
        );
    }

    #[test]
    fn most_surprising_domains_sorted_correctly() {
        let mut state = FreeEnergyState::new(100);

        let id = state.predict("calm_domain", 0.5, 0.9);
        state.observe(&id, 0.51);

        let id = state.predict("wild_domain", 0.5, 0.9);
        state.observe(&id, 0.99);

        let top = state.most_surprising_domains(2);
        assert_eq!(top[0].0, "wild_domain");
        assert!(top[0].1 > top[1].1);
    }

    #[test]
    fn free_energy_decreases_with_better_predictions() {
        let mut state = FreeEnergyState::new(100);

        let id = state.predict("topic", 0.5, 0.8);
        state.observe(&id, 0.95);
        let high_fe = state.free_energy();

        let id = state.predict("topic", 0.5, 0.8);
        state.observe(&id, 0.51);
        let lower_fe = state.free_energy();

        assert!(
            lower_fe < high_fe,
            "free energy should decrease: {lower_fe} < {high_fe}"
        );
    }

    #[test]
    fn should_update_model_triggers_at_threshold() {
        let mut state = FreeEnergyState::new(100);
        let id = state.predict("volatile", 0.5, 0.9);
        state.observe(&id, 0.99);

        assert!(state.should_update_model("volatile", 0.5));
        assert!(!state.should_update_model("volatile", 50.0));
        assert!(!state.should_update_model("nonexistent", 0.0));
    }

    #[test]
    fn should_act_triggers_on_high_free_energy() {
        let mut state = FreeEnergyState::new(100);

        for _ in 0..5 {
            let id = state.predict("chaos", 0.1, 0.5);
            state.observe(&id, 0.95);
        }

        assert!(state.should_act(0.5));
        assert!(!state.should_act(100.0));
    }

    #[test]
    fn ring_buffer_prunes_old_entries() {
        let mut state = FreeEnergyState::new(3);

        for i in 0..5 {
            let id = state.predict("test", f64::from(i), 0.5);
            state.observe(&id, f64::from(i) + 0.1);
        }

        assert!(state.predictions.len() <= 3);
        assert!(state.errors.len() <= 3);
    }

    #[test]
    fn reset_domain_clears_only_target() {
        let mut state = FreeEnergyState::new(100);

        let id_a = state.predict("alpha", 0.5, 0.9);
        state.observe(&id_a, 0.6);
        let id_b = state.predict("beta", 0.5, 0.9);
        state.observe(&id_b, 0.7);

        state.reset_domain("alpha");

        assert!(state.accuracy("alpha").is_none());
        assert!(state.accuracy("beta").is_some());
        assert!(state.domain_surprise("alpha").is_none());
        assert!(state.domain_surprise("beta").is_some());
    }

    #[test]
    fn observe_returns_none_for_unknown_prediction() {
        let mut state = FreeEnergyState::new(100);
        assert!(state.observe("nonexistent", 1.0).is_none());
    }

    #[test]
    fn free_energy_zero_with_no_errors() {
        let state = FreeEnergyState::new(100);
        assert!((state.free_energy() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn confidence_clamped_to_valid_range() {
        let mut state = FreeEnergyState::new(100);
        state.predict("test", 0.5, -0.5);
        state.predict("test", 0.5, 1.5);

        assert!((state.predictions[0].confidence - 0.0).abs() < f32::EPSILON);
        assert!((state.predictions[1].confidence - 1.0).abs() < f32::EPSILON);
    }
}
