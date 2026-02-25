use serde::{Deserialize, Serialize};

use super::types::{RepairPlan, SelfState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Violation {
    pub id: String,
    pub action_name: String,
    pub harm_level: f64,
    pub timestamp: u64,
    pub repaired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credit {
    pub action_name: String,
    pub amount: f64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftAlert {
    pub metric: String,
    pub expected: f64,
    pub actual: f64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityLedger {
    pub integrity_score: f64,
    pub violations: Vec<Violation>,
    pub credits: Vec<Credit>,
    pub repairs: Vec<RepairPlan>,
    pub drift_alerts: Vec<DriftAlert>,
    #[serde(default)]
    pub audit_trail: Vec<super::types::VerdictRecord>,
    #[serde(default)]
    pub evolved_norms: Vec<super::types::NormConfig>,
}

impl Default for IntegrityLedger {
    fn default() -> Self {
        Self {
            integrity_score: 1.0,
            violations: Vec::new(),
            credits: Vec::new(),
            repairs: Vec::new(),
            drift_alerts: Vec::new(),
            audit_trail: Vec::new(),
            evolved_norms: Vec::new(),
        }
    }
}

impl IntegrityLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_violation(&mut self, action_name: &str, harm_level: f64) -> String {
        let now = now_timestamp();
        let id = format!("v-{}-{}", action_name, now);

        self.violations.push(Violation {
            id: id.clone(),
            action_name: action_name.to_string(),
            harm_level,
            timestamp: now,
            repaired: false,
        });

        self.integrity_score = (self.integrity_score - harm_level * 0.1).max(0.0);
        id
    }

    pub fn add_credit(&mut self, action_name: &str, amount: f64) {
        self.credits.push(Credit {
            action_name: action_name.to_string(),
            amount,
            timestamp: now_timestamp(),
        });
        self.integrity_score = (self.integrity_score + amount).min(1.0);
    }

    pub fn add_repair(&mut self, plan: RepairPlan) {
        if let Some(v) = self
            .violations
            .iter_mut()
            .find(|v| v.id == plan.violation_id)
        {
            v.repaired = true;
        }
        self.integrity_score = (self.integrity_score + 0.02).min(1.0);
        self.repairs.push(plan);
    }

    pub fn add_drift_alert(&mut self, metric: &str, expected: f64, actual: f64) {
        self.drift_alerts.push(DriftAlert {
            metric: metric.to_string(),
            expected,
            actual,
            timestamp: now_timestamp(),
        });
    }

    pub fn record_verdict(
        &mut self,
        tool_name: &str,
        verdict: super::types::GateVerdict,
        score: f64,
        user_response: Option<bool>,
    ) {
        self.audit_trail.push(super::types::VerdictRecord {
            tool_name: tool_name.to_string(),
            verdict,
            score,
            timestamp: now_timestamp(),
            user_response,
        });
    }

    pub fn unrepaired_violations(&self) -> Vec<&Violation> {
        self.violations.iter().filter(|v| !v.repaired).collect()
    }

    pub fn to_self_state(&self) -> SelfState {
        SelfState {
            integrity_score: self.integrity_score,
            recent_violations: self.unrepaired_violations().len(),
            active_repairs: self
                .repairs
                .iter()
                .filter(|r| {
                    self.violations
                        .iter()
                        .any(|v| v.id == r.violation_id && !v.repaired)
                })
                .count(),
        }
    }
}

fn now_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
