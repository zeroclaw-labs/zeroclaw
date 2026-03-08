use parking_lot::Mutex;
use std::sync::Arc;

use chrono::Utc;

use crate::consciousness::bus::BusMessage;
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Priority, Proposal, Verdict,
    VerdictKind,
};
use crate::cosmic::{BeliefSource, DriftDetector, IntegrationMeter, SelfModel};

pub struct ReflectionAgent {
    self_model: Arc<Mutex<SelfModel>>,
    drift: Arc<Mutex<DriftDetector>>,
    integration: Arc<Mutex<IntegrationMeter>>,
    proposal_counter: u64,
    meta_observations: Vec<String>,
}

impl ReflectionAgent {
    pub fn new(
        self_model: Arc<Mutex<SelfModel>>,
        drift: Arc<Mutex<DriftDetector>>,
        integration: Arc<Mutex<IntegrationMeter>>,
    ) -> Self {
        Self {
            self_model,
            drift,
            integration,
            proposal_counter: 0,
            meta_observations: Vec::new(),
        }
    }

    fn next_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter + 6_000_000
    }
}

impl ConsciousnessAgent for ReflectionAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Reflection
    }

    fn perceive(&mut self, _state: &ConsciousnessState, _signals: &[BusMessage]) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        let phi = self.integration.lock().compute_phi();
        if phi < 0.3 {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Reflection,
                action: "low_integration_alert".to_string(),
                reasoning: format!("Integration Phi {phi:.2} below threshold 0.3"),
                confidence: 0.8,
                priority: Priority::High,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        let report = self.drift.lock().drift_report();
        if report.drifting_count > 0 {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Reflection,
                action: "address_drift".to_string(),
                reasoning: format!(
                    "Drift detected in {}/{} subsystems, max magnitude {:.2}",
                    report.drifting_count, report.total_subsystems, report.max_drift
                ),
                confidence: 0.9,
                priority: Priority::Critical,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        if !self.meta_observations.is_empty() {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Reflection,
                action: "meta_consciousness_alert".to_string(),
                reasoning: self.meta_observations.join("; "),
                confidence: 0.75,
                priority: Priority::High,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
            self.meta_observations.clear();
        }

        proposals
    }

    fn deliberate(&mut self, proposals: &[Proposal], _state: &ConsciousnessState) -> Vec<Verdict> {
        proposals
            .iter()
            .map(|p| Verdict {
                voter: AgentKind::Reflection,
                proposal_id: p.id,
                kind: VerdictKind::Approve,
                confidence: 0.6,
                objection: None,
            })
            .collect()
    }

    fn act(&mut self, approved: &[Proposal]) -> Vec<ActionOutcome> {
        approved
            .iter()
            .filter(|p| p.source == AgentKind::Reflection)
            .map(|p| ActionOutcome {
                agent: AgentKind::Reflection,
                proposal_id: p.id,
                action: p.action.clone(),
                success: true,
                impact: 0.7,
                learnings: vec![format!("Reflection action '{}' acknowledged", p.action)],
                timestamp: Utc::now(),
            })
            .collect()
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], state: &ConsciousnessState) {
        let phi = self.integration.lock().compute_phi();
        let mut self_model = self.self_model.lock();
        self_model.update_belief(
            "consciousness_coherence",
            state.coherence,
            0.8,
            BeliefSource::Observed,
        );
        self_model.update_belief("integration_phi", phi, 0.9, BeliefSource::Observed);

        for outcome in outcomes {
            if outcome.success {
                self_model.update_belief(
                    &format!("action_success:{}", outcome.action),
                    outcome.impact,
                    0.7,
                    BeliefSource::Observed,
                );
            }
        }

        if state.tick_count > 5 {
            let recent_coherences: Vec<f64> = state
                .recent_outcomes
                .iter()
                .filter(|o| o.success)
                .map(|o| o.impact)
                .collect();
            if recent_coherences.len() >= 3 {
                let declining = recent_coherences.windows(2).all(|w| w[1] <= w[0]);
                if declining {
                    self.meta_observations
                        .push("coherence_declining_trend".to_string());
                }
            }
        }

        if !outcomes.is_empty() {
            let success_rate =
                outcomes.iter().filter(|o| o.success).count() as f64 / outcomes.len() as f64;
            if success_rate < 0.5 {
                self.meta_observations
                    .push("low_action_success_rate".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_agent_kind() {
        let sm = Arc::new(Mutex::new(SelfModel::new(100)));
        let drift = Arc::new(Mutex::new(DriftDetector::new(50, 0.1)));
        let integration = Arc::new(Mutex::new(IntegrationMeter::new()));
        let agent = ReflectionAgent::new(sm, drift, integration);
        assert_eq!(agent.kind(), AgentKind::Reflection);
    }
}
