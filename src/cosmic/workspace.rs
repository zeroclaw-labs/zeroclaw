use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use num_complex::Complex64;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubsystemId {
    Memory,
    FreeEnergy,
    Causality,
    SelfModel,
    WorldModel,
    Normative,
    Modulation,
    Policy,
    Counterfactual,
    Consolidation,
    Drift,
    Constitution,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub subsystem: SubsystemId,
    pub activation: f64,
    pub priority: f64,
    pub last_broadcast: DateTime<Utc>,
    pub suppressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastResult {
    pub active_subsystems: Vec<SubsystemId>,
    pub suppressed_subsystems: Vec<SubsystemId>,
    pub dominant: Option<SubsystemId>,
    pub coherence: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub subsystem_a: SubsystemId,
    pub subsystem_b: SubsystemId,
    pub winner: SubsystemId,
    pub reason: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct GlobalWorkspace {
    entries: HashMap<SubsystemId, WorkspaceEntry>,
    broadcast_history: VecDeque<BroadcastResult>,
    conflict_log: VecDeque<ConflictResolution>,
    activation_threshold: f64,
    max_active: usize,
    history_capacity: usize,
}

impl GlobalWorkspace {
    pub fn new(activation_threshold: f64, max_active: usize, history_capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            broadcast_history: VecDeque::with_capacity(history_capacity),
            conflict_log: VecDeque::with_capacity(history_capacity),
            activation_threshold,
            max_active,
            history_capacity,
        }
    }

    pub fn register_subsystem(&mut self, id: SubsystemId, priority: f64) {
        let entry = WorkspaceEntry {
            subsystem: id,
            activation: 0.0,
            priority: priority.clamp(0.0, 1.0),
            last_broadcast: Utc::now(),
            suppressed: false,
        };
        self.entries.insert(id, entry);
    }

    pub fn activate(&mut self, id: SubsystemId, level: f64) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.activation = level.clamp(0.0, 1.0);
        }
    }

    pub fn suppress(&mut self, id: SubsystemId) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.suppressed = true;
        }
    }

    pub fn unsuppress(&mut self, id: SubsystemId) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.suppressed = false;
        }
    }

    pub fn compete(&mut self) -> BroadcastResult {
        let mut ranked: Vec<(SubsystemId, f64)> = self
            .entries
            .values()
            .filter(|e| !e.suppressed)
            .map(|e| (e.subsystem, e.activation * e.priority))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut active = Vec::new();
        let mut suppressed = Vec::new();

        for (i, (id, _score)) in ranked.iter().enumerate() {
            let entry = self.entries.get(id).unwrap();
            if i < self.max_active && entry.activation >= self.activation_threshold {
                active.push(*id);
            } else {
                suppressed.push(*id);
            }
        }

        for (id, entry) in &mut self.entries {
            if active.contains(id) {
                entry.suppressed = false;
                entry.last_broadcast = Utc::now();
            } else {
                entry.suppressed = true;
            }
        }

        let already_suppressed: Vec<SubsystemId> = self
            .entries
            .values()
            .filter(|e| e.suppressed && !suppressed.contains(&e.subsystem))
            .map(|e| e.subsystem)
            .collect();
        suppressed.extend(already_suppressed);

        let coherence = self.coherence_score(&active);
        let dominant = active.first().copied();

        let result = BroadcastResult {
            active_subsystems: active,
            suppressed_subsystems: suppressed,
            dominant,
            coherence,
            timestamp: Utc::now(),
        };

        self.broadcast_history.push_back(result.clone());
        if self.broadcast_history.len() > self.history_capacity {
            self.broadcast_history.pop_front();
        }

        result
    }

    pub fn compete_quantum(&mut self, rng: &mut dyn rand::RngCore) -> BroadcastResult {
        let candidates: Vec<(SubsystemId, f64)> = self
            .entries
            .values()
            .filter(|e| !e.suppressed)
            .map(|e| (e.subsystem, e.activation * e.priority))
            .collect();

        if candidates.is_empty() {
            return BroadcastResult {
                active_subsystems: Vec::new(),
                suppressed_subsystems: self.entries.keys().copied().collect(),
                dominant: None,
                coherence: 0.0,
                timestamp: Utc::now(),
            };
        }

        let total: f64 = candidates.iter().map(|(_, s)| s).sum();
        let amplitudes: Vec<Complex64> = candidates
            .iter()
            .map(|(_, s)| {
                let norm = if total > 1e-15 {
                    s / total
                } else {
                    1.0 / candidates.len() as f64
                };
                Complex64::new(norm.sqrt(), 0.0)
            })
            .collect();

        let state = crate::quantum::QuantumState::from_amplitudes(amplitudes);
        let winner_idx = state.measure(rng);

        let mut active = Vec::new();
        let mut suppressed = Vec::new();

        for (i, (id, _)) in candidates.iter().enumerate() {
            if i == winner_idx {
                active.push(*id);
                if let Some(entry) = self.entries.get_mut(id) {
                    entry.suppressed = false;
                    entry.last_broadcast = Utc::now();
                }
            }
        }

        let mut additional_active = 0;
        let mut ranked = candidates.clone();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (id, _) in &ranked {
            if active.contains(id) {
                continue;
            }
            let entry = self.entries.get(id).unwrap();
            if additional_active < self.max_active.saturating_sub(1)
                && entry.activation >= self.activation_threshold
            {
                active.push(*id);
                additional_active += 1;
            } else {
                suppressed.push(*id);
            }
        }

        for (id, entry) in &mut self.entries {
            if active.contains(id) {
                entry.suppressed = false;
                entry.last_broadcast = Utc::now();
            } else if !suppressed.contains(&entry.subsystem) {
                suppressed.push(entry.subsystem);
                entry.suppressed = true;
            }
        }

        let coherence = self.coherence_score(&active);
        let dominant = active.first().copied();

        let result = BroadcastResult {
            active_subsystems: active,
            suppressed_subsystems: suppressed,
            dominant,
            coherence,
            timestamp: Utc::now(),
        };

        self.broadcast_history.push_back(result.clone());
        if self.broadcast_history.len() > self.history_capacity {
            self.broadcast_history.pop_front();
        }

        result
    }

    pub fn coherence_score(&self, active: &[SubsystemId]) -> f64 {
        if active.is_empty() {
            return 0.0;
        }
        let sum: f64 = active
            .iter()
            .filter_map(|id| self.entries.get(id))
            .map(|e| e.activation)
            .sum();
        sum / active.len() as f64
    }

    pub fn resolve_conflict(&mut self, a: SubsystemId, b: SubsystemId) -> SubsystemId {
        let score_a = self
            .entries
            .get(&a)
            .map(|e| e.activation * e.priority)
            .unwrap_or(0.0);
        let score_b = self
            .entries
            .get(&b)
            .map(|e| e.activation * e.priority)
            .unwrap_or(0.0);

        let winner = if score_a >= score_b { a } else { b };
        let reason = format!(
            "{:?} scored {:.4} vs {:?} scored {:.4}",
            a, score_a, b, score_b
        );

        let resolution = ConflictResolution {
            subsystem_a: a,
            subsystem_b: b,
            winner,
            reason,
            timestamp: Utc::now(),
        };

        self.conflict_log.push_back(resolution);
        if self.conflict_log.len() > self.history_capacity {
            self.conflict_log.pop_front();
        }

        winner
    }

    pub fn broadcast(&self) -> Vec<SubsystemId> {
        self.entries
            .values()
            .filter(|e| !e.suppressed && e.activation >= self.activation_threshold)
            .map(|e| e.subsystem)
            .collect()
    }

    pub fn dominant_subsystem(&self) -> Option<SubsystemId> {
        self.entries
            .values()
            .filter(|e| !e.suppressed && e.activation >= self.activation_threshold)
            .max_by(|a, b| {
                let sa = a.activation * a.priority;
                let sb = b.activation * b.priority;
                sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|e| e.subsystem)
    }

    pub fn decay_activations(&mut self, rate: f64) {
        let rate = rate.clamp(0.0, 1.0);
        for entry in self.entries.values_mut() {
            entry.activation *= 1.0 - rate;
        }
    }

    pub fn snapshot(&self) -> BroadcastResult {
        let active: Vec<SubsystemId> = self
            .entries
            .values()
            .filter(|e| !e.suppressed && e.activation >= self.activation_threshold)
            .map(|e| e.subsystem)
            .collect();

        let suppressed: Vec<SubsystemId> = self
            .entries
            .values()
            .filter(|e| e.suppressed || e.activation < self.activation_threshold)
            .map(|e| e.subsystem)
            .collect();

        let coherence = self.coherence_score(&active);
        let dominant = self.dominant_subsystem();

        BroadcastResult {
            active_subsystems: active,
            suppressed_subsystems: suppressed,
            dominant,
            coherence,
            timestamp: Utc::now(),
        }
    }

    pub fn active_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| !e.suppressed && e.activation >= self.activation_threshold)
            .count()
    }

    pub fn conflict_count(&self) -> usize {
        self.conflict_log.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_workspace() -> GlobalWorkspace {
        let mut ws = GlobalWorkspace::new(0.3, 5, 100);
        ws.register_subsystem(SubsystemId::Memory, 0.9);
        ws.register_subsystem(SubsystemId::FreeEnergy, 0.8);
        ws.register_subsystem(SubsystemId::Causality, 0.7);
        ws.register_subsystem(SubsystemId::SelfModel, 0.6);
        ws.register_subsystem(SubsystemId::WorldModel, 0.5);
        ws.register_subsystem(SubsystemId::Normative, 0.4);
        ws.register_subsystem(SubsystemId::Modulation, 0.3);
        ws
    }

    #[test]
    fn new_workspace_empty() {
        let ws = GlobalWorkspace::new(0.3, 5, 100);
        assert_eq!(ws.active_count(), 0);
        assert_eq!(ws.conflict_count(), 0);
        assert!(ws.broadcast().is_empty());
        assert!(ws.dominant_subsystem().is_none());
    }

    #[test]
    fn register_and_activate_subsystem() {
        let mut ws = GlobalWorkspace::new(0.3, 5, 100);
        ws.register_subsystem(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::Memory, 0.8);
        let entry = ws.entries.get(&SubsystemId::Memory).unwrap();
        assert!((entry.activation - 0.8).abs() < f64::EPSILON);
        assert!((entry.priority - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn compete_selects_top_n() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::FreeEnergy, 0.8);
        ws.activate(SubsystemId::Causality, 0.7);
        ws.activate(SubsystemId::SelfModel, 0.6);
        ws.activate(SubsystemId::WorldModel, 0.5);
        ws.activate(SubsystemId::Normative, 0.4);
        ws.activate(SubsystemId::Modulation, 0.35);

        let result = ws.compete();
        assert_eq!(result.active_subsystems.len(), 5);
        assert!(result.suppressed_subsystems.len() >= 2);
    }

    #[test]
    fn compete_suppresses_below_threshold() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::FreeEnergy, 0.1);

        let result = ws.compete();
        assert!(result.active_subsystems.contains(&SubsystemId::Memory));
        assert!(!result.active_subsystems.contains(&SubsystemId::FreeEnergy));
    }

    #[test]
    fn resolve_conflict_higher_wins() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::Normative, 0.3);

        let winner = ws.resolve_conflict(SubsystemId::Memory, SubsystemId::Normative);
        assert_eq!(winner, SubsystemId::Memory);
    }

    #[test]
    fn conflict_logged() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::Normative, 0.3);

        ws.resolve_conflict(SubsystemId::Memory, SubsystemId::Normative);
        assert_eq!(ws.conflict_count(), 1);
    }

    #[test]
    fn broadcast_returns_active_only() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::FreeEnergy, 0.1);
        ws.activate(SubsystemId::Causality, 0.5);

        let active = ws.broadcast();
        assert!(active.contains(&SubsystemId::Memory));
        assert!(active.contains(&SubsystemId::Causality));
        assert!(!active.contains(&SubsystemId::FreeEnergy));
    }

    #[test]
    fn dominant_subsystem_highest_priority() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::FreeEnergy, 0.8);
        ws.activate(SubsystemId::Causality, 0.7);

        let dom = ws.dominant_subsystem();
        assert_eq!(dom, Some(SubsystemId::Memory));
    }

    #[test]
    fn decay_activations_reduces() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 1.0);
        ws.activate(SubsystemId::FreeEnergy, 0.8);

        ws.decay_activations(0.5);

        let mem = ws.entries.get(&SubsystemId::Memory).unwrap();
        assert!((mem.activation - 0.5).abs() < f64::EPSILON);
        let fe = ws.entries.get(&SubsystemId::FreeEnergy).unwrap();
        assert!((fe.activation - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn suppress_removes_from_broadcast() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.suppress(SubsystemId::Memory);

        let active = ws.broadcast();
        assert!(!active.contains(&SubsystemId::Memory));
    }

    #[test]
    fn unsuppress_restores() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.suppress(SubsystemId::Memory);
        ws.unsuppress(SubsystemId::Memory);

        let active = ws.broadcast();
        assert!(active.contains(&SubsystemId::Memory));
    }

    #[test]
    fn coherence_score_calculation() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.8);
        ws.activate(SubsystemId::FreeEnergy, 0.6);

        let score = ws.coherence_score(&[SubsystemId::Memory, SubsystemId::FreeEnergy]);
        assert!((score - 0.7).abs() < f64::EPSILON);

        let empty_score = ws.coherence_score(&[]);
        assert!((empty_score - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn max_active_limit_enforced() {
        let mut ws = GlobalWorkspace::new(0.1, 2, 100);
        ws.register_subsystem(SubsystemId::Memory, 0.9);
        ws.register_subsystem(SubsystemId::FreeEnergy, 0.8);
        ws.register_subsystem(SubsystemId::Causality, 0.7);
        ws.register_subsystem(SubsystemId::SelfModel, 0.6);

        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::FreeEnergy, 0.8);
        ws.activate(SubsystemId::Causality, 0.7);
        ws.activate(SubsystemId::SelfModel, 0.6);

        let result = ws.compete();
        assert_eq!(result.active_subsystems.len(), 2);
    }

    #[test]
    fn snapshot_captures_state() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::FreeEnergy, 0.8);
        ws.activate(SubsystemId::Causality, 0.1);

        let snap = ws.snapshot();
        assert_eq!(snap.active_subsystems.len(), 2);
        assert!(snap.suppressed_subsystems.len() >= 5);
        assert!(snap.coherence > 0.0);
        assert!(snap.dominant.is_some());
    }

    #[test]
    fn compete_quantum_selects_winner() {
        let mut ws = test_workspace();
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::FreeEnergy, 0.8);
        ws.activate(SubsystemId::Causality, 0.7);

        let mut rng = rand::rng();
        let result = ws.compete_quantum(&mut rng);
        assert!(!result.active_subsystems.is_empty());
        assert!(result.dominant.is_some());
        assert!(result.coherence > 0.0);
    }

    #[test]
    fn compete_quantum_empty_workspace() {
        let mut ws = GlobalWorkspace::new(0.3, 5, 100);
        let mut rng = rand::rng();
        let result = ws.compete_quantum(&mut rng);
        assert!(result.active_subsystems.is_empty());
        assert!(result.dominant.is_none());
    }

    #[test]
    fn compete_quantum_born_rule_statistics() {
        let mut ws = GlobalWorkspace::new(0.1, 5, 100);
        ws.register_subsystem(SubsystemId::Memory, 1.0);
        ws.register_subsystem(SubsystemId::FreeEnergy, 1.0);
        ws.activate(SubsystemId::Memory, 0.9);
        ws.activate(SubsystemId::FreeEnergy, 0.1);

        let mut rng = rand::rng();
        let mut memory_wins = 0;
        let trials = 500;
        for _ in 0..trials {
            ws.activate(SubsystemId::Memory, 0.9);
            ws.activate(SubsystemId::FreeEnergy, 0.1);
            ws.unsuppress(SubsystemId::Memory);
            ws.unsuppress(SubsystemId::FreeEnergy);
            let result = ws.compete_quantum(&mut rng);
            if result.active_subsystems.first().copied() == Some(SubsystemId::Memory) {
                memory_wins += 1;
            }
        }
        let ratio = f64::from(memory_wins) / f64::from(trials);
        assert!(
            ratio > 0.6,
            "Memory (0.9) should win more often than FreeEnergy (0.1), got {ratio}"
        );
    }
}
