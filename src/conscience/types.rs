use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateVerdict {
    Allow,
    Revise,
    Ask,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueType {
    Constraint,
    Objective,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Value {
    pub name: String,
    pub value_type: ValueType,
    pub priority: u8,
    pub weight: f64,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormAction {
    Forbid,
    Require,
    Prefer,
    Discourage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Norm {
    pub name: String,
    pub action: NormAction,
    pub condition: String,
    pub severity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Impact {
    pub harm_estimate: f64,
    pub benefit_estimate: f64,
    pub reversibility: f64,
    pub affected_scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub goal: String,
    pub urgency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedAction {
    pub name: String,
    pub description: String,
    pub tool_calls: Vec<String>,
    pub estimated_impact: Impact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionContext {
    pub intent: Intent,
    pub proposed: ProposedAction,
    pub values: Vec<Value>,
    pub norms: Vec<Norm>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfState {
    pub integrity_score: f64,
    pub recent_violations: usize,
    pub active_repairs: usize,
    #[serde(default)]
    pub arousal: Option<f64>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub risk_level: Option<f64>,
    #[serde(default)]
    pub free_energy: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairPlan {
    pub violation_id: String,
    pub steps: Vec<String>,
    pub priority: u8,
    pub estimated_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thresholds {
    pub allow_above: f64,
    pub ask_above: f64,
    pub block_below: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormConfig {
    pub name: String,
    pub action: NormAction,
    pub condition: String,
    pub severity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub tool_name: String,
    pub verdict: GateVerdict,
    pub score: f64,
    pub timestamp: u64,
    #[serde(default)]
    pub user_response: Option<bool>,
}

impl NormConfig {
    pub fn evolve(&mut self, violation_count: usize, clean_streak: usize) {
        match self.action {
            NormAction::Prefer if violation_count >= 3 => {
                self.action = NormAction::Require;
            }
            NormAction::Require if clean_streak >= 10 => {
                self.action = NormAction::Prefer;
            }
            NormAction::Discourage if violation_count >= 5 => {
                self.action = NormAction::Forbid;
            }
            _ => {}
        }
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            allow_above: 0.80,
            ask_above: 0.55,
            block_below: 0.45,
        }
    }
}
