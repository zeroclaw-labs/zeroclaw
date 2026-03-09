use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SomaticMarker {
    pub marker_type: String,
    pub intensity: f64,
    pub trigger: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeostaticDrive {
    pub drive_name: String,
    pub current_level: f64,
    pub set_point: f64,
    pub urgency: f64,
}

impl HomeostaticDrive {
    pub fn compute_urgency(&self) -> f64 {
        (self.current_level - self.set_point).abs().min(1.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPerceptionCycle {
    pub action: String,
    pub outcome_success: bool,
    pub phenomenal: super::traits::PhenomenalState,
    pub tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnactiveLoop {
    cycles: Vec<ActionPerceptionCycle>,
    capacity: usize,
}

impl EnactiveLoop {
    pub fn new(capacity: usize) -> Self {
        Self {
            cycles: Vec::with_capacity(capacity),
            capacity,
        }
    }

    pub fn record_action_perception_cycle(
        &mut self,
        action: &str,
        outcome_success: bool,
        phenomenal: super::traits::PhenomenalState,
        tick: u64,
    ) {
        if self.cycles.len() >= self.capacity {
            self.cycles.remove(0);
        }
        self.cycles.push(ActionPerceptionCycle {
            action: action.to_string(),
            outcome_success,
            phenomenal,
            tick,
        });
    }

    pub fn recent_cycles(&self) -> &[ActionPerceptionCycle] {
        &self.cycles
    }

    pub fn action_success_rate(&self, action: &str) -> f64 {
        let matching: Vec<&ActionPerceptionCycle> =
            self.cycles.iter().filter(|c| c.action == action).collect();
        if matching.is_empty() {
            return 0.0;
        }
        let successes = matching.iter().filter(|c| c.outcome_success).count();
        successes as f64 / matching.len() as f64
    }
}

impl Default for EnactiveLoop {
    fn default() -> Self {
        Self::new(256)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TheoryOfMind {
    pub agent_id: String,
    pub believed_state: String,
    pub confidence: f64,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutobiographicalMemory {
    pub episode_id: u64,
    pub context: String,
    pub outcome: String,
    pub emotional_valence: f64,
    pub tick: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowState {
    pub in_flow: bool,
    pub flow_duration: u64,
    pub entry_tick: Option<u64>,
}
