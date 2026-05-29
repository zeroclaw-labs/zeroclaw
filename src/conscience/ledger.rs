use serde::{Deserialize, Serialize};
use std::path::Path;

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

    /// Read a serialised ledger from `path`. Returns `Self::default()`
    /// (i.e. a fresh healthy ledger) when the file is missing, empty,
    /// or unparseable — fresh-start semantics rather than failing closed.
    /// The error path is intentionally silent here; callers that need a
    /// hard failure should call [`Self::load`] (TODO when wanted).
    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) if !bytes.is_empty() => {
                serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "path": path.display().to_string(),
                                "error": e.to_string(),
                            })),
                        "IntegrityLedger: corrupt persistence file, starting fresh"
                    );
                    Self::default()
                })
            }
            _ => Self::default(),
        }
    }

    /// Atomically persist this ledger to `path`. Writes to `path.tmp`
    /// first, then renames so partial writes don't leave the file in
    /// a half-serialised state. Creates the parent directory if missing.
    /// Returns an `io::Error` on filesystem failure; callers in the hot
    /// path typically `.ok()` it so a temporarily-unwritable disk
    /// doesn't break tool dispatch.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
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
            arousal: None,
            confidence: None,
            risk_level: None,
            free_energy: None,
        }
    }
}

fn now_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
