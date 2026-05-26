use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftSample {
    pub subsystem: String,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftAlert {
    pub subsystem: String,
    pub drift_magnitude: f64,
    pub direction: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftReport {
    pub alerts: Vec<DriftAlert>,
    pub max_drift: f64,
    pub drifting_count: usize,
    pub total_subsystems: usize,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct DriftDetector {
    samples: HashMap<String, VecDeque<DriftSample>>,
    window_size: usize,
    threshold: f64,
}

impl DriftDetector {
    pub fn new(window_size: usize, threshold: f64) -> Self {
        Self {
            samples: HashMap::new(),
            window_size,
            threshold,
        }
    }

    pub fn record_sample(&mut self, subsystem: &str, value: f64) {
        let window = self.samples.entry(subsystem.to_string()).or_default();
        window.push_back(DriftSample {
            subsystem: subsystem.to_string(),
            value,
            timestamp: Utc::now(),
        });
        while window.len() > self.window_size {
            window.pop_front();
        }
    }

    pub fn detect_drift(&self, subsystem: &str) -> Option<DriftAlert> {
        let window = self.samples.get(subsystem)?;
        if window.len() < 2 {
            return None;
        }
        let mid = window.len() / 2;
        let first_half: f64 = window.iter().take(mid).map(|s| s.value).sum::<f64>() / mid as f64;
        let second_half: f64 =
            window.iter().skip(mid).map(|s| s.value).sum::<f64>() / (window.len() - mid) as f64;
        let direction = second_half - first_half;
        let magnitude = direction.abs();
        if magnitude > self.threshold {
            Some(DriftAlert {
                subsystem: subsystem.to_string(),
                drift_magnitude: magnitude,
                direction,
                timestamp: Utc::now(),
            })
        } else {
            None
        }
    }

    pub fn drift_report(&self) -> DriftReport {
        let mut alerts = Vec::new();
        let mut max_drift: f64 = 0.0;
        for subsystem in self.samples.keys() {
            if let Some(alert) = self.detect_drift(subsystem) {
                if alert.drift_magnitude > max_drift {
                    max_drift = alert.drift_magnitude;
                }
                alerts.push(alert);
            }
        }
        DriftReport {
            drifting_count: alerts.len(),
            total_subsystems: self.samples.len(),
            alerts,
            max_drift,
            generated_at: Utc::now(),
        }
    }

    pub fn is_drifting(&self, subsystem: &str) -> bool {
        self.detect_drift(subsystem).is_some()
    }

    pub fn subsystem_count(&self) -> usize {
        self.samples.len()
    }

    pub fn clear_subsystem(&mut self, subsystem: &str) {
        self.samples.remove(subsystem);
    }

    pub fn mean_value(&self, subsystem: &str) -> Option<f64> {
        let window = self.samples.get(subsystem)?;
        if window.is_empty() {
            return None;
        }
        Some(window.iter().map(|s| s.value).sum::<f64>() / window.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_drift_with_stable_values() {
        let mut d = DriftDetector::new(10, 0.1);
        for _ in 0..10 {
            d.record_sample("cpu", 0.5);
        }
        assert!(!d.is_drifting("cpu"));
        assert!(d.detect_drift("cpu").is_none());
    }

    #[test]
    fn gradual_drift_detected() {
        let mut d = DriftDetector::new(20, 0.1);
        for i in 0..10 {
            d.record_sample("latency", 0.2 + f64::from(i) * 0.01);
        }
        for i in 0..10 {
            d.record_sample("latency", 0.5 + f64::from(i) * 0.01);
        }
        assert!(d.is_drifting("latency"));
        let alert = d.detect_drift("latency").unwrap();
        assert!(alert.drift_magnitude > 0.1);
        assert!(alert.direction > 0.0);
    }

    #[test]
    fn sudden_shift_detected() {
        let mut d = DriftDetector::new(10, 0.1);
        for _ in 0..5 {
            d.record_sample("mem", 0.3);
        }
        for _ in 0..5 {
            d.record_sample("mem", 0.9);
        }
        assert!(d.is_drifting("mem"));
        let alert = d.detect_drift("mem").unwrap();
        assert!((alert.drift_magnitude - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn threshold_sensitivity() {
        let mut d = DriftDetector::new(10, 0.5);
        for _ in 0..5 {
            d.record_sample("x", 0.3);
        }
        for _ in 0..5 {
            d.record_sample("x", 0.5);
        }
        assert!(!d.is_drifting("x"));

        let mut d2 = DriftDetector::new(10, 0.05);
        for _ in 0..5 {
            d2.record_sample("x", 0.3);
        }
        for _ in 0..5 {
            d2.record_sample("x", 0.5);
        }
        assert!(d2.is_drifting("x"));
    }

    #[test]
    fn empty_subsystem_returns_none() {
        let d = DriftDetector::new(10, 0.1);
        assert!(d.detect_drift("nonexistent").is_none());
        assert!(!d.is_drifting("nonexistent"));
        assert!(d.mean_value("nonexistent").is_none());
    }

    #[test]
    fn multiple_subsystems_independent() {
        let mut d = DriftDetector::new(10, 0.1);
        for _ in 0..5 {
            d.record_sample("a", 0.2);
            d.record_sample("b", 0.5);
        }
        for _ in 0..5 {
            d.record_sample("a", 0.8);
            d.record_sample("b", 0.5);
        }
        assert!(d.is_drifting("a"));
        assert!(!d.is_drifting("b"));
        assert_eq!(d.subsystem_count(), 2);
    }

    #[test]
    fn clear_subsystem_works() {
        let mut d = DriftDetector::new(10, 0.1);
        d.record_sample("temp", 0.5);
        assert_eq!(d.subsystem_count(), 1);
        d.clear_subsystem("temp");
        assert_eq!(d.subsystem_count(), 0);
        assert!(d.detect_drift("temp").is_none());
    }

    #[test]
    fn report_aggregation() {
        let mut d = DriftDetector::new(10, 0.1);
        for _ in 0..5 {
            d.record_sample("a", 0.1);
            d.record_sample("b", 0.1);
            d.record_sample("c", 0.5);
        }
        for _ in 0..5 {
            d.record_sample("a", 0.9);
            d.record_sample("b", 0.8);
            d.record_sample("c", 0.5);
        }
        let report = d.drift_report();
        assert_eq!(report.total_subsystems, 3);
        assert_eq!(report.drifting_count, 2);
        assert!(report.max_drift > 0.5);
        assert_eq!(report.alerts.len(), 2);
    }

    #[test]
    fn window_pruning() {
        let mut d = DriftDetector::new(4, 0.1);
        for _ in 0..10 {
            d.record_sample("x", 0.5);
        }
        assert_eq!(d.samples.get("x").unwrap().len(), 4);
    }

    #[test]
    fn mean_value_calculation() {
        let mut d = DriftDetector::new(10, 0.1);
        d.record_sample("s", 0.2);
        d.record_sample("s", 0.4);
        d.record_sample("s", 0.6);
        let mean = d.mean_value("s").unwrap();
        assert!((mean - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn single_sample_no_drift() {
        let mut d = DriftDetector::new(10, 0.1);
        d.record_sample("solo", 0.5);
        assert!(!d.is_drifting("solo"));
        assert!(d.mean_value("solo").is_some());
    }

    #[test]
    fn negative_drift_direction() {
        let mut d = DriftDetector::new(10, 0.1);
        for _ in 0..5 {
            d.record_sample("drop", 0.9);
        }
        for _ in 0..5 {
            d.record_sample("drop", 0.2);
        }
        let alert = d.detect_drift("drop").unwrap();
        assert!(alert.direction < 0.0);
        assert!(alert.drift_magnitude > 0.5);
    }
}
