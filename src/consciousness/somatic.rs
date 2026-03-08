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
pub struct EnactiveLoop {
    pub perception: String,
    pub action_tendency: String,
    pub coupling_strength: f64,
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
