use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PhenomenalState {
    pub attention: f64,
    pub arousal: f64,
    pub valence: f64,
}

impl Default for PhenomenalState {
    fn default() -> Self {
        Self {
            attention: 0.5,
            arousal: 0.5,
            valence: 0.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TemporalNarrative {
    pub past_intentions: Vec<String>,
    pub current_intention: String,
    pub future_intentions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentKind {
    Chairman,
    Memory,
    Research,
    Strategy,
    Execution,
    Conscience,
    Reflection,
    Metacognitive,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chairman => write!(f, "Chairman"),
            Self::Memory => write!(f, "Memory"),
            Self::Research => write!(f, "Research"),
            Self::Strategy => write!(f, "Strategy"),
            Self::Execution => write!(f, "Execution"),
            Self::Conscience => write!(f, "Conscience"),
            Self::Reflection => write!(f, "Reflection"),
            Self::Metacognitive => write!(f, "Metacognitive"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerdictKind {
    Approve,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    Low,
    Normal,
    High,
    Critical,
}

impl Priority {
    pub fn weight(&self) -> f64 {
        match self {
            Self::Low => 0.25,
            Self::Normal => 0.5,
            Self::High => 0.75,
            Self::Critical => 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: u64,
    pub source: AgentKind,
    pub action: String,
    pub reasoning: String,
    pub confidence: f64,
    pub priority: Priority,
    pub contradicts: Vec<u64>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub voter: AgentKind,
    pub proposal_id: u64,
    pub kind: VerdictKind,
    pub confidence: f64,
    pub objection: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionOutcome {
    pub agent: AgentKind,
    pub proposal_id: u64,
    pub action: String,
    pub success: bool,
    pub impact: f64,
    pub learnings: Vec<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contradiction {
    pub proposal_a: u64,
    pub proposal_b: u64,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContradictionResolution {
    pub contradiction: Contradiction,
    pub winner: u64,
    pub loser: u64,
    pub winner_score: f64,
    pub loser_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsciousnessState {
    pub coherence: f64,
    pub tick_count: u64,
    pub active_proposals: Vec<Proposal>,
    pub recent_outcomes: Vec<ActionOutcome>,
    pub timestamp: DateTime<Utc>,
    pub phenomenal: PhenomenalState,
    pub narrative: TemporalNarrative,
    pub somatic_markers: Vec<super::somatic::SomaticMarker>,
    pub homeostatic_drives: Vec<super::somatic::HomeostaticDrive>,
    pub enactive_loop: Option<super::somatic::EnactiveLoop>,
    pub theory_of_mind: Vec<super::somatic::TheoryOfMind>,
    pub autobiographical_memory: Vec<super::somatic::AutobiographicalMemory>,
    pub flow_state: super::somatic::FlowState,
}

impl Default for ConsciousnessState {
    fn default() -> Self {
        Self {
            coherence: 1.0,
            tick_count: 0,
            active_proposals: Vec::new(),
            recent_outcomes: Vec::new(),
            timestamp: Utc::now(),
            phenomenal: PhenomenalState::default(),
            narrative: TemporalNarrative::default(),
            somatic_markers: Vec::new(),
            homeostatic_drives: Vec::new(),
            enactive_loop: None,
            theory_of_mind: Vec::new(),
            autobiographical_memory: Vec::new(),
            flow_state: super::somatic::FlowState::default(),
        }
    }
}

pub trait ConsciousnessAgent: Send + Sync {
    fn kind(&self) -> AgentKind;

    fn perceive(
        &mut self,
        state: &ConsciousnessState,
        signals: &[super::bus::BusMessage],
    ) -> Vec<Proposal>;

    fn deliberate(&mut self, proposals: &[Proposal], state: &ConsciousnessState) -> Vec<Verdict>;

    fn act(&mut self, approved: &[Proposal]) -> Vec<ActionOutcome>;

    fn reflect(&mut self, outcomes: &[ActionOutcome], state: &ConsciousnessState);

    fn vote_weight(&self) -> f64 {
        0.5
    }

    fn phenomenal_state(&self) -> PhenomenalState {
        PhenomenalState::default()
    }

    fn update_phenomenal(&mut self, _outcomes: &[ActionOutcome], _state: &ConsciousnessState) {}

    fn somatic_markers(&self) -> Vec<super::somatic::SomaticMarker> {
        Vec::new()
    }

    fn autobiographical_episodes(&self) -> Vec<super::somatic::AutobiographicalMemory> {
        Vec::new()
    }
}
