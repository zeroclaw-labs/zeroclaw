use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatentState {
    pub dims: Vec<f64>,
    pub timestamp: DateTime<Utc>,
}

impl LatentState {
    pub fn new(dims: Vec<f64>) -> Self {
        Self {
            dims,
            timestamp: Utc::now(),
        }
    }

    pub fn zeros(dim: usize) -> Self {
        Self::new(vec![0.0; dim])
    }

    pub fn dim(&self) -> usize {
        self.dims.len()
    }

    pub fn distance(&self, other: &Self) -> f64 {
        self.dims
            .iter()
            .zip(other.dims.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f64>()
            .sqrt()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictedObservation {
    pub values: HashMap<String, f64>,
    pub confidence: f64,
}

pub trait WorldModelEncoder: Send + Sync {
    fn encode(&self, signals: &[Signal], entities: &HashMap<String, EntityState>) -> LatentState;
}

pub trait LatentDynamics: Send + Sync {
    fn predict(&self, state: &LatentState, action: &str) -> LatentState;
}

pub trait WorldModelDecoder: Send + Sync {
    fn decode(&self, state: &LatentState) -> PredictedObservation;
}

pub trait RewardModel: Send + Sync {
    fn compute_reward(
        &self,
        predicted: &PredictedObservation,
        actual: &HashMap<String, f64>,
    ) -> f64;
}

pub struct DefaultEncoder {
    pub latent_dim: usize,
}

impl WorldModelEncoder for DefaultEncoder {
    fn encode(&self, signals: &[Signal], entities: &HashMap<String, EntityState>) -> LatentState {
        let mut dims = vec![0.0; self.latent_dim];
        if !signals.is_empty() {
            dims[0] = signals.iter().map(|s| s.relevance).sum::<f64>() / signals.len() as f64;
        }
        if self.latent_dim > 1 {
            dims[1] = entities.len() as f64 / 100.0;
        }
        if self.latent_dim > 2 {
            dims[2] = signals.len() as f64 / 1000.0;
        }
        LatentState::new(dims)
    }
}

pub struct DefaultDynamics {
    pub decay: f64,
}

impl LatentDynamics for DefaultDynamics {
    fn predict(&self, state: &LatentState, _action: &str) -> LatentState {
        let dims: Vec<f64> = state.dims.iter().map(|d| d * self.decay).collect();
        LatentState::new(dims)
    }
}

pub struct DefaultDecoder;

impl WorldModelDecoder for DefaultDecoder {
    fn decode(&self, state: &LatentState) -> PredictedObservation {
        let mut values = HashMap::new();
        for (i, d) in state.dims.iter().enumerate() {
            values.insert(format!("dim_{i}"), *d);
        }
        let confidence = if state.dims.is_empty() {
            0.0
        } else {
            state.dims.iter().map(|d| d.abs()).sum::<f64>() / state.dims.len() as f64
        };
        PredictedObservation {
            values,
            confidence: confidence.clamp(0.0, 1.0),
        }
    }
}

pub struct DefaultRewardModel;

impl RewardModel for DefaultRewardModel {
    fn compute_reward(
        &self,
        predicted: &PredictedObservation,
        actual: &HashMap<String, f64>,
    ) -> f64 {
        if actual.is_empty() {
            return 0.0;
        }
        let mut error_sum = 0.0;
        let mut count = 0;
        for (key, actual_val) in actual {
            if let Some(pred_val) = predicted.values.get(key) {
                error_sum += (pred_val - actual_val).abs();
                count += 1;
            }
        }
        if count == 0 {
            return 0.0;
        }
        let mean_error = error_sum / f64::from(count);
        (1.0 - mean_error).clamp(-1.0, 1.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub source: String,
    pub content: String,
    pub relevance: f64,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub description: String,
    pub severity: f64,
    pub detected_at: DateTime<Utc>,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityState {
    pub name: String,
    pub properties: HashMap<String, String>,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorldModel {
    entities: HashMap<String, EntityState>,
    signals: Vec<Signal>,
    anomalies: Vec<Anomaly>,
    last_updated: Option<DateTime<Utc>>,
}

impl WorldModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest_signal(&mut self, source: String, content: String, relevance: f64) {
        self.signals.push(Signal {
            source,
            content,
            relevance: relevance.clamp(0.0, 1.0),
            received_at: Utc::now(),
        });
        self.last_updated = Some(Utc::now());
        if self.signals.len() > 10_000 {
            self.signals.remove(0);
        }
    }

    pub fn register_anomaly(&mut self, description: String, severity: f64) {
        self.anomalies.push(Anomaly {
            description,
            severity: severity.clamp(0.0, 1.0),
            detected_at: Utc::now(),
            resolved: false,
        });
    }

    pub fn resolve_anomaly(&mut self, index: usize) -> bool {
        if let Some(a) = self.anomalies.get_mut(index) {
            a.resolved = true;
            true
        } else {
            false
        }
    }

    pub fn update_entity(&mut self, name: String, properties: HashMap<String, String>) {
        self.entities.insert(
            name.clone(),
            EntityState {
                name,
                properties,
                last_updated: Utc::now(),
            },
        );
        self.last_updated = Some(Utc::now());
    }

    pub fn entity(&self, name: &str) -> Option<&EntityState> {
        self.entities.get(name)
    }

    pub fn recent_signals(&self, count: usize) -> &[Signal] {
        let start = self.signals.len().saturating_sub(count);
        &self.signals[start..]
    }

    pub fn unresolved_anomalies(&self) -> Vec<&Anomaly> {
        self.anomalies.iter().filter(|a| !a.resolved).collect()
    }

    pub fn all_anomalies(&self) -> &[Anomaly] {
        &self.anomalies
    }

    pub fn signal_count(&self) -> usize {
        self.signals.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_signal_and_retrieve() {
        let mut wm = WorldModel::new();
        wm.ingest_signal("sensor_a".into(), "temp=22".into(), 0.7);
        assert_eq!(wm.signal_count(), 1);
        assert_eq!(wm.recent_signals(10)[0].source, "sensor_a");
    }

    #[test]
    fn anomaly_lifecycle() {
        let mut wm = WorldModel::new();
        wm.register_anomaly("spike".into(), 0.9);
        assert_eq!(wm.unresolved_anomalies().len(), 1);
        wm.resolve_anomaly(0);
        assert_eq!(wm.unresolved_anomalies().len(), 0);
    }

    #[test]
    fn entity_update_and_lookup() {
        let mut wm = WorldModel::new();
        let mut props = HashMap::new();
        props.insert("status".into(), "online".into());
        wm.update_entity("node_a".into(), props);
        let e = wm.entity("node_a").unwrap();
        assert_eq!(e.properties.get("status").unwrap(), "online");
    }

    #[test]
    fn latent_state_serde_roundtrip() {
        let state = LatentState::new(vec![0.1, 0.5, 0.9]);
        let json = serde_json::to_string(&state).unwrap();
        let parsed: LatentState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.dims.len(), 3);
        assert!((parsed.dims[0] - 0.1).abs() < f64::EPSILON);
        assert!((parsed.dims[2] - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn latent_state_distance() {
        let a = LatentState::new(vec![0.0, 0.0]);
        let b = LatentState::new(vec![3.0, 4.0]);
        assert!((a.distance(&b) - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn latent_state_zeros() {
        let z = LatentState::zeros(8);
        assert_eq!(z.dim(), 8);
        assert!(z.dims.iter().all(|d| *d == 0.0));
    }

    #[test]
    fn default_encoder_produces_latent() {
        let encoder = DefaultEncoder { latent_dim: 4 };
        let signals = vec![Signal {
            source: "s1".into(),
            content: "data".into(),
            relevance: 0.8,
            received_at: Utc::now(),
        }];
        let entities = HashMap::new();
        let latent = encoder.encode(&signals, &entities);
        assert_eq!(latent.dim(), 4);
        assert!((latent.dims[0] - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn default_dynamics_decays_state() {
        let dynamics = DefaultDynamics { decay: 0.9 };
        let state = LatentState::new(vec![1.0, 0.5]);
        let next = dynamics.predict(&state, "act");
        assert!((next.dims[0] - 0.9).abs() < f64::EPSILON);
        assert!((next.dims[1] - 0.45).abs() < f64::EPSILON);
    }

    #[test]
    fn default_decoder_produces_observation() {
        let decoder = DefaultDecoder;
        let state = LatentState::new(vec![0.3, 0.7]);
        let obs = decoder.decode(&state);
        assert_eq!(obs.values.len(), 2);
        assert!(obs.confidence > 0.0);
    }

    #[test]
    fn default_reward_model_perfect_prediction() {
        let reward_model = DefaultRewardModel;
        let mut predicted_values = HashMap::new();
        predicted_values.insert("dim_0".into(), 0.5);
        let predicted = PredictedObservation {
            values: predicted_values,
            confidence: 0.9,
        };
        let mut actual = HashMap::new();
        actual.insert("dim_0".into(), 0.5);
        let reward = reward_model.compute_reward(&predicted, &actual);
        assert!((reward - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn default_reward_model_bad_prediction() {
        let reward_model = DefaultRewardModel;
        let mut predicted_values = HashMap::new();
        predicted_values.insert("dim_0".into(), 0.0);
        let predicted = PredictedObservation {
            values: predicted_values,
            confidence: 0.5,
        };
        let mut actual = HashMap::new();
        actual.insert("dim_0".into(), 1.0);
        let reward = reward_model.compute_reward(&predicted, &actual);
        assert!((reward - 0.0).abs() < f64::EPSILON);
    }
}
