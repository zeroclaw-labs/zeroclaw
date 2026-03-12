use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub proficiency: f64,
    pub last_exercised: Option<DateTime<Utc>>,
    pub success_count: u64,
    pub failure_count: u64,
}

impl Capability {
    pub fn success_rate(&self) -> f64 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            return 0.0;
        }
        self.success_count as f64 / total as f64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limitation {
    pub name: String,
    pub severity: f64,
    pub discovered_at: DateTime<Utc>,
    pub workaround: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetric {
    pub name: String,
    pub value: f64,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SelfModel {
    capabilities: HashMap<String, Capability>,
    limitations: Vec<Limitation>,
    metrics_history: Vec<PerformanceMetric>,
    overall_confidence: f64,
}

impl SelfModel {
    pub fn new() -> Self {
        Self {
            overall_confidence: 0.5,
            ..Default::default()
        }
    }

    pub fn register_capability(&mut self, name: String, proficiency: f64) {
        self.capabilities.insert(
            name.clone(),
            Capability {
                name,
                proficiency: proficiency.clamp(0.0, 1.0),
                last_exercised: None,
                success_count: 0,
                failure_count: 0,
            },
        );
    }

    pub fn record_outcome(&mut self, capability: &str, success: bool) {
        if let Some(cap) = self.capabilities.get_mut(capability) {
            if success {
                cap.success_count += 1;
            } else {
                cap.failure_count += 1;
            }
            cap.last_exercised = Some(Utc::now());
            cap.proficiency = cap.success_rate();
        }
    }

    pub fn add_limitation(&mut self, name: String, severity: f64, workaround: Option<String>) {
        self.limitations.push(Limitation {
            name,
            severity: severity.clamp(0.0, 1.0),
            discovered_at: Utc::now(),
            workaround,
        });
    }

    pub fn record_metric(&mut self, name: String, value: f64) {
        self.metrics_history.push(PerformanceMetric {
            name,
            value,
            recorded_at: Utc::now(),
        });
        if self.metrics_history.len() > 10_000 {
            self.metrics_history.remove(0);
        }
    }

    pub fn self_assessment(&self) -> f64 {
        if self.capabilities.is_empty() {
            return self.overall_confidence;
        }
        let avg_proficiency: f64 = self
            .capabilities
            .values()
            .map(|c| c.proficiency)
            .sum::<f64>()
            / self.capabilities.len() as f64;

        let limitation_penalty = (self.limitations.len() as f64 * 0.05).min(0.5);
        (avg_proficiency - limitation_penalty).clamp(0.0, 1.0)
    }

    pub fn capabilities(&self) -> impl Iterator<Item = &Capability> {
        self.capabilities.values()
    }

    pub fn limitations(&self) -> &[Limitation] {
        &self.limitations
    }

    pub fn recent_metrics(&self, count: usize) -> &[PerformanceMetric] {
        let start = self.metrics_history.len().saturating_sub(count);
        &self.metrics_history[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_record_outcomes() {
        let mut model = SelfModel::new();
        model.register_capability("coding".into(), 0.5);
        model.record_outcome("coding", true);
        model.record_outcome("coding", true);
        model.record_outcome("coding", false);
        let cap = model.capabilities.get("coding").unwrap();
        assert_eq!(cap.success_count, 2);
        assert_eq!(cap.failure_count, 1);
        assert!((cap.success_rate() - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn self_assessment_accounts_for_limitations() {
        let mut model = SelfModel::new();
        model.register_capability("a".into(), 0.9);
        let high = model.self_assessment();
        model.add_limitation("slow".into(), 0.5, None);
        let lower = model.self_assessment();
        assert!(lower < high);
    }

    #[test]
    fn empty_model_returns_default_confidence() {
        let model = SelfModel::new();
        assert!((model.self_assessment() - 0.5).abs() < f64::EPSILON);
    }
}
