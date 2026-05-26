use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvironmentModel {
    pub state_vars: HashMap<String, f64>,
    pub predictions: HashMap<String, f64>,
    pub last_updated: u64,
    pub source_id: String,
    pub confidence: f64,
}

impl EnvironmentModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, key: &str, value: f64, tick: u64) {
        self.state_vars.insert(key.to_string(), value);
        self.last_updated = tick;
    }

    pub fn predict(&mut self, key: &str, value: f64) {
        self.predictions.insert(key.to_string(), value);
    }

    pub fn divergence(&self) -> f64 {
        let mut total_error = 0.0;
        let mut count = 0;
        for (key, &predicted) in &self.predictions {
            if let Some(&actual) = self.state_vars.get(key) {
                total_error += (predicted - actual).abs();
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            total_error / f64::from(count)
        }
    }

    pub fn get(&self, key: &str) -> Option<f64> {
        self.state_vars.get(key).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_tracking_and_prediction_error() {
        let mut env = EnvironmentModel::new();
        env.observe("temp", 22.0, 1);
        env.predict("temp", 25.0);
        let div = env.divergence();
        assert!((div - 3.0).abs() < f64::EPSILON);
        assert_eq!(env.get("temp"), Some(22.0));
        assert_eq!(env.last_updated, 1);
    }

    #[test]
    fn no_predictions_zero_divergence() {
        let mut env = EnvironmentModel::new();
        env.observe("x", 1.0, 1);
        assert_eq!(env.divergence(), 0.0);
    }
}
