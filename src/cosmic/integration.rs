use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemState {
    pub name: String,
    pub value: f64,
    pub connections: Vec<String>,
    pub last_update: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationSnapshot {
    pub phi: f64,
    pub subsystem_count: usize,
    pub edge_count: usize,
    pub hub_ratio: f64,
    pub clustering_coefficient: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct IntegrationMeter {
    subsystems: HashMap<String, SubsystemState>,
}

impl IntegrationMeter {
    pub fn new() -> Self {
        Self {
            subsystems: HashMap::new(),
        }
    }

    pub fn register_subsystem(&mut self, name: &str, connections: Vec<String>) {
        self.subsystems.insert(
            name.to_string(),
            SubsystemState {
                name: name.to_string(),
                value: 0.0,
                connections,
                last_update: Utc::now(),
            },
        );
    }

    pub fn update_state(&mut self, name: &str, value: f64) {
        if let Some(state) = self.subsystems.get_mut(name) {
            state.value = value;
            state.last_update = Utc::now();
        }
    }

    pub fn compute_phi(&self) -> f64 {
        let pairs = self.connected_pairs();
        if pairs.is_empty() {
            return 0.0;
        }
        let total: f64 = pairs
            .iter()
            .map(|(a, b)| {
                let va = self.subsystems[a].value;
                let vb = self.subsystems[b].value;
                1.0 - (va - vb).abs()
            })
            .sum();
        total / pairs.len() as f64
    }

    pub fn hub_ratio(&self) -> f64 {
        if self.subsystems.is_empty() {
            return 0.0;
        }
        let mut degrees: Vec<usize> = self
            .subsystems
            .values()
            .map(|s| s.connections.len())
            .collect();
        degrees.sort_unstable_by(|a, b| b.cmp(a));

        let total_edges: usize = degrees.iter().sum();
        if total_edges == 0 {
            return 0.0;
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let top_count = (self.subsystems.len() as f64 * 0.2).ceil() as usize;
        let top_count = top_count.max(1);
        let top_edges: usize = degrees.iter().take(top_count).sum();

        top_edges as f64 / total_edges as f64
    }

    pub fn clustering_coefficient(&self) -> f64 {
        if self.subsystems.is_empty() {
            return 0.0;
        }

        let mut total = 0.0;
        let mut counted = 0usize;

        for state in self.subsystems.values() {
            let neighbors = &state.connections;
            let k = neighbors.len();
            if k < 2 {
                continue;
            }

            let mut triangles = 0usize;
            for i in 0..k {
                for j in (i + 1)..k {
                    if let Some(ni) = self.subsystems.get(&neighbors[i]) {
                        if ni.connections.contains(&neighbors[j]) {
                            triangles += 1;
                        }
                    }
                }
            }

            let possible = k * (k - 1) / 2;
            total += triangles as f64 / possible as f64;
            counted += 1;
        }

        if counted == 0 {
            return 0.0;
        }
        total / counted as f64
    }

    pub fn snapshot(&self) -> IntegrationSnapshot {
        IntegrationSnapshot {
            phi: self.compute_phi(),
            subsystem_count: self.subsystem_count(),
            edge_count: self.edge_count(),
            hub_ratio: self.hub_ratio(),
            clustering_coefficient: self.clustering_coefficient(),
            timestamp: Utc::now(),
        }
    }

    pub fn is_scale_free(&self) -> bool {
        self.hub_ratio() >= 0.6
    }

    pub fn weakest_link(&self) -> Option<(String, String, f64)> {
        let pairs = self.connected_pairs();
        pairs
            .into_iter()
            .map(|(a, b)| {
                let va = self.subsystems[&a].value;
                let vb = self.subsystems[&b].value;
                let mi = 1.0 - (va - vb).abs();
                (a, b, mi)
            })
            .min_by(|x, y| x.2.partial_cmp(&y.2).unwrap_or(std::cmp::Ordering::Equal))
    }

    pub fn subsystem_count(&self) -> usize {
        self.subsystems.len()
    }

    fn edge_count(&self) -> usize {
        let mut count = 0usize;
        for (name, state) in &self.subsystems {
            for conn in &state.connections {
                if conn > name && self.subsystems.contains_key(conn) {
                    count += 1;
                }
            }
        }
        count
    }

    fn connected_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for (name, state) in &self.subsystems {
            for conn in &state.connections {
                if conn > name && self.subsystems.contains_key(conn) {
                    pairs.push((name.clone(), conn.clone()));
                }
            }
        }
        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle_meter() -> IntegrationMeter {
        let mut m = IntegrationMeter::new();
        m.register_subsystem("a", vec!["b".into(), "c".into()]);
        m.register_subsystem("b", vec!["a".into(), "c".into()]);
        m.register_subsystem("c", vec!["a".into(), "b".into()]);
        m
    }

    #[test]
    fn register_subsystems_count() {
        let mut m = IntegrationMeter::new();
        m.register_subsystem("alpha", vec!["beta".into()]);
        m.register_subsystem("beta", vec!["alpha".into()]);
        assert_eq!(m.subsystem_count(), 2);
    }

    #[test]
    fn phi_identical_states_equals_one() {
        let mut m = triangle_meter();
        m.update_state("a", 0.5);
        m.update_state("b", 0.5);
        m.update_state("c", 0.5);
        let phi = m.compute_phi();
        assert!((phi - 1.0).abs() < 1e-10);
    }

    #[test]
    fn phi_divergent_states_less_than_one() {
        let mut m = triangle_meter();
        m.update_state("a", 0.0);
        m.update_state("b", 1.0);
        m.update_state("c", 0.5);
        let phi = m.compute_phi();
        assert!(phi < 1.0);
        assert!(phi > 0.0);
    }

    #[test]
    fn hub_ratio_scale_free_structure() {
        let mut m = IntegrationMeter::new();
        m.register_subsystem(
            "hub",
            vec![
                "s1".into(),
                "s2".into(),
                "s3".into(),
                "s4".into(),
                "s5".into(),
            ],
        );
        for i in 1..=5 {
            m.register_subsystem(&format!("s{i}"), vec!["hub".into()]);
        }
        let ratio = m.hub_ratio();
        assert!(ratio >= 0.6, "hub_ratio {ratio} should indicate scale-free");
        assert!(m.is_scale_free());
    }

    #[test]
    fn clustering_coefficient_triangle() {
        let m = triangle_meter();
        let cc = m.clustering_coefficient();
        assert!(
            (cc - 1.0).abs() < 1e-10,
            "triangle should have cc=1.0, got {cc}"
        );
    }

    #[test]
    fn weakest_link_most_divergent_pair() {
        let mut m = IntegrationMeter::new();
        m.register_subsystem("x", vec!["y".into(), "z".into()]);
        m.register_subsystem("y", vec!["x".into()]);
        m.register_subsystem("z", vec!["x".into()]);
        m.update_state("x", 0.0);
        m.update_state("y", 0.1);
        m.update_state("z", 1.0);

        let (a, b, mi) = m.weakest_link().unwrap();
        assert!(
            (a == "x" && b == "z") || (a == "z" && b == "x"),
            "weakest link should be (x,z), got ({a},{b})"
        );
        assert!((mi - 0.0).abs() < 1e-10);
    }

    #[test]
    fn empty_meter_phi_zero() {
        let m = IntegrationMeter::new();
        assert!((m.compute_phi() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn snapshot_populates_all_fields() {
        let mut m = triangle_meter();
        m.update_state("a", 0.3);
        m.update_state("b", 0.3);
        m.update_state("c", 0.3);
        let snap = m.snapshot();
        assert_eq!(snap.subsystem_count, 3);
        assert_eq!(snap.edge_count, 3);
        assert!((snap.phi - 1.0).abs() < 1e-10);
        assert!((snap.clustering_coefficient - 1.0).abs() < 1e-10);
        assert!(snap.hub_ratio >= 0.0 && snap.hub_ratio <= 1.0);
        assert!(snap.timestamp <= Utc::now());
    }

    #[test]
    fn hub_ratio_uniform_topology() {
        let m = triangle_meter();
        let ratio = m.hub_ratio();
        assert!(
            !m.is_scale_free(),
            "uniform triangle should not be scale-free, ratio={ratio}"
        );
    }
}
