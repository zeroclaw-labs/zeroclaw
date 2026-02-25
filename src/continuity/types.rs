use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityCore {
    pub name: String,
    pub constitution_hash: String,
    pub creation_epoch: u64,
    pub immutable_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub core: IdentityCore,
    pub preferences: Vec<Preference>,
    pub narrative: Vec<Episode>,
    pub commitments: Vec<Commitment>,
    pub session_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftLimits {
    pub max_daily: f64,
    pub max_session: f64,
}

impl Default for DriftLimits {
    fn default() -> Self {
        Self {
            max_daily: 0.10,
            max_session: 0.05,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreferenceCategory {
    Communication,
    Technical,
    Ethical,
    Aesthetic,
    Social,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub key: String,
    pub value: String,
    pub confidence: f64,
    pub category: PreferenceCategory,
    pub last_updated: u64,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub evolution_history: Vec<PreferenceSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub summary: String,
    pub timestamp: u64,
    pub significance: f64,
    pub verified: bool,
    pub tags: Vec<String>,
    #[serde(default)]
    pub emotional_tag: Option<String>,
    #[serde(default)]
    pub valence_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    pub description: String,
    pub made_at: u64,
    pub expires_at: Option<u64>,
    pub fulfilled: bool,
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuitySnapshot {
    pub identity: Identity,
    pub drift_limits: DriftLimits,
    pub snapshot_timestamp: u64,
    pub session_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferenceSnapshot {
    pub value: String,
    pub confidence: f64,
    pub timestamp: u64,
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferenceDelta {
    pub key: String,
    pub old_value: String,
    pub new_value: String,
    pub old_confidence: f64,
    pub new_confidence: f64,
    pub drift_amount: f64,
    pub reasoning: Option<String>,
    pub timestamp: u64,
}
