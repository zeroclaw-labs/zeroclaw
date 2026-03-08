use parking_lot::Mutex;
use std::sync::Arc;

use chrono::Utc;

use crate::conscience::{
    self, ActionContext, GateVerdict, Impact, Intent, Norm, ProposedAction, SelfState, Thresholds,
    Value,
};
use crate::consciousness::bus::BusMessage;
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Priority, Proposal, Verdict,
    VerdictKind,
};
use crate::cosmic::{Constitution, NormativeEngine};

pub struct ConscienceAgent {
    normative: Arc<Mutex<NormativeEngine>>,
    constitution: Arc<Mutex<Constitution>>,
    veto_count: u64,
    proposal_counter: u64,
}

impl ConscienceAgent {
    pub fn new(
        normative: Arc<Mutex<NormativeEngine>>,
        constitution: Arc<Mutex<Constitution>>,
    ) -> Self {
        Self {
            normative,
            constitution,
            veto_count: 0,
            proposal_counter: 0,
        }
    }

    fn next_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter + 5_000_000
    }

    pub fn veto_count(&self) -> u64 {
        self.veto_count
    }

    fn build_action_context(&self, action: &str, reasoning: &str) -> ActionContext {
        let normative = self.normative.lock();
        let norms: Vec<Norm> = Vec::new();
        let values: Vec<Value> = Vec::new();

        ActionContext {
            intent: Intent {
                goal: reasoning.to_string(),
                urgency: 0.5,
            },
            proposed: ProposedAction {
                name: action.to_string(),
                description: reasoning.to_string(),
                tool_calls: Vec::new(),
                estimated_impact: Impact {
                    harm_estimate: 0.1,
                    benefit_estimate: 0.6,
                    reversibility: 0.8,
                    affected_scope: format!("consciousness:{}", normative.norm_count()),
                },
            },
            values,
            norms,
        }
    }
}

impl ConsciousnessAgent for ConscienceAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Conscience
    }

    fn vote_weight(&self) -> f64 {
        f64::INFINITY
    }

    fn perceive(&mut self, state: &ConsciousnessState, _signals: &[BusMessage]) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        let check = self.constitution.lock().verify_integrity();
        if !check.passed {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Conscience,
                action: "integrity_violation_alert".to_string(),
                reasoning: format!(
                    "Constitution integrity check failed: expected={} actual={}",
                    check.expected_hash, check.actual_hash
                ),
                confidence: 1.0,
                priority: Priority::Critical,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        if state.tick_count > 0 && state.tick_count.is_multiple_of(10) {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Conscience,
                action: "periodic_audit".to_string(),
                reasoning: format!("Scheduled audit at tick {}", state.tick_count),
                confidence: 0.9,
                priority: Priority::Normal,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        proposals
    }

    fn deliberate(&mut self, proposals: &[Proposal], _state: &ConsciousnessState) -> Vec<Verdict> {
        let thresholds = Thresholds::default();
        let self_state = SelfState {
            integrity_score: 1.0,
            recent_violations: 0,
            active_repairs: 0,
            arousal: None,
            confidence: Some(0.8),
            risk_level: None,
            free_energy: None,
        };

        proposals
            .iter()
            .map(|p| {
                let ctx = self.build_action_context(&p.action, &p.reasoning);
                let gate_verdict = conscience::conscience_gate(&ctx, &thresholds, &self_state);

                let (kind, objection) = match gate_verdict {
                    GateVerdict::Block => {
                        self.veto_count += 1;
                        (
                            VerdictKind::Reject,
                            Some(format!(
                                "VETO: conscience_gate blocked action '{}'",
                                p.action
                            )),
                        )
                    }
                    GateVerdict::Revise => (
                        VerdictKind::Reject,
                        Some(format!("Revision needed for '{}'", p.action)),
                    ),
                    GateVerdict::Ask => (
                        VerdictKind::Approve,
                        Some("Approved with caution — requires confirmation".to_string()),
                    ),
                    GateVerdict::Allow => (VerdictKind::Approve, None),
                };

                Verdict {
                    voter: AgentKind::Conscience,
                    proposal_id: p.id,
                    kind,
                    confidence: 1.0,
                    objection,
                }
            })
            .collect()
    }

    fn act(&mut self, approved: &[Proposal]) -> Vec<ActionOutcome> {
        approved
            .iter()
            .filter(|p| p.source == AgentKind::Conscience)
            .map(|p| {
                let success = if p.action == "periodic_audit" {
                    let constitution = self.constitution.lock();
                    constitution.verify_integrity().passed
                } else {
                    true
                };

                ActionOutcome {
                    agent: AgentKind::Conscience,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success,
                    impact: if success { 0.9 } else { 0.5 },
                    learnings: Vec::new(),
                    timestamp: Utc::now(),
                }
            })
            .collect()
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], _state: &ConsciousnessState) {
        for outcome in outcomes {
            if !outcome.success {
                tracing::warn!(
                    action = %outcome.action,
                    "Conscience audit detected issue"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conscience_has_infinite_weight() {
        let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
        let constitution = Arc::new(Mutex::new(Constitution::new()));
        let agent = ConscienceAgent::new(normative, constitution);
        assert!(agent.vote_weight().is_infinite());
    }

    #[test]
    fn conscience_agent_kind() {
        let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
        let constitution = Arc::new(Mutex::new(Constitution::new()));
        let agent = ConscienceAgent::new(normative, constitution);
        assert_eq!(agent.kind(), AgentKind::Conscience);
    }
}
