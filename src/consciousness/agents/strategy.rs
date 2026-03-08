use parking_lot::Mutex;
use std::sync::Arc;

use chrono::Utc;

use crate::consciousness::bus::BusMessage;
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Priority, Proposal, Verdict,
    VerdictKind,
};
use crate::cosmic::{CounterfactualEngine, FreeEnergyState, PolicyEngine, Scenario};

pub struct StrategyAgent {
    counterfactual: Arc<Mutex<CounterfactualEngine>>,
    policy: Arc<Mutex<PolicyEngine>>,
    free_energy: Arc<Mutex<FreeEnergyState>>,
    proposal_counter: u64,
    goals: Vec<String>,
}

impl StrategyAgent {
    pub fn new(
        counterfactual: Arc<Mutex<CounterfactualEngine>>,
        policy: Arc<Mutex<PolicyEngine>>,
        free_energy: Arc<Mutex<FreeEnergyState>>,
    ) -> Self {
        Self {
            counterfactual,
            policy,
            free_energy,
            proposal_counter: 0,
            goals: Vec::new(),
        }
    }

    fn next_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter + 3_000_000
    }

    pub fn counterfactual_imagination(&self, action: &str) -> Vec<Proposal> {
        let mut cf = self.counterfactual.lock();
        let scenario = Scenario {
            id: format!("whatif_{action}"),
            action: action.to_string(),
            context: std::collections::HashMap::new(),
            created_at: Utc::now(),
        };
        let result = cf.simulate(&scenario);
        if result.confidence > 0.5 {
            vec![Proposal {
                id: 0,
                source: AgentKind::Strategy,
                action: format!("counterfactual:{action}"),
                reasoning: format!("What-if simulation confidence {:.2}", result.confidence),
                confidence: result.confidence,
                priority: Priority::Normal,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            }]
        } else {
            Vec::new()
        }
    }
}

impl ConsciousnessAgent for StrategyAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Strategy
    }

    fn perceive(&mut self, _state: &ConsciousnessState, _signals: &[BusMessage]) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        let surprise = self.free_energy.lock().free_energy();

        if surprise > 0.5 {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Strategy,
                action: "reduce_surprise".to_string(),
                reasoning: format!("Free energy {surprise:.2} exceeds threshold"),
                confidence: 0.7,
                priority: if surprise > 0.8 {
                    Priority::Critical
                } else {
                    Priority::High
                },
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        if let Some(goal) = self.goals.first().cloned() {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Strategy,
                action: format!("pursue_goal:{goal}"),
                reasoning: format!("Emergent goal: {goal}"),
                confidence: 0.6,
                priority: Priority::Normal,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        proposals
    }

    fn deliberate(&mut self, proposals: &[Proposal], _state: &ConsciousnessState) -> Vec<Verdict> {
        proposals
            .iter()
            .map(|p| {
                let mut policy = self.policy.lock();
                let decision = policy.evaluate(&p.action, "consciousness");
                let kind = if decision.score >= 0.0 {
                    VerdictKind::Approve
                } else {
                    VerdictKind::Reject
                };

                Verdict {
                    voter: AgentKind::Strategy,
                    proposal_id: p.id,
                    kind,
                    confidence: (decision.score.abs()).min(1.0),
                    objection: if kind == VerdictKind::Reject {
                        Some(format!(
                            "Policy evaluation negative: score={:.2}",
                            decision.score
                        ))
                    } else {
                        None
                    },
                }
            })
            .collect()
    }

    fn act(&mut self, approved: &[Proposal]) -> Vec<ActionOutcome> {
        approved
            .iter()
            .filter(|p| p.source == AgentKind::Strategy)
            .map(|p| {
                let success = if p.action == "reduce_surprise" {
                    let mut cf = self.counterfactual.lock();
                    let scenario = Scenario {
                        id: format!("consciousness_{}", p.id),
                        action: p.action.clone(),
                        context: std::collections::HashMap::new(),
                        created_at: Utc::now(),
                    };
                    let result = cf.simulate(&scenario);
                    result.confidence > 0.3
                } else {
                    true
                };

                ActionOutcome {
                    agent: AgentKind::Strategy,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success,
                    impact: if success { 0.7 } else { 0.0 },
                    learnings: Vec::new(),
                    timestamp: Utc::now(),
                }
            })
            .collect()
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], state: &ConsciousnessState) {
        for outcome in outcomes {
            if outcome.success
                && (outcome.action.contains("expand") || outcome.action.contains("optimize"))
            {
                let goal = "continue_expansion".to_string();
                if !self.goals.contains(&goal) {
                    self.goals.push(goal);
                }
            }
        }

        if state.coherence > 0.8 {
            let goal = "maintain_stability".to_string();
            if !self.goals.contains(&goal) {
                self.goals.push(goal);
            }
        }

        self.goals.truncate(5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_agent_kind() {
        let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 10)));
        let policy = Arc::new(Mutex::new(PolicyEngine::new(10)));
        let fe = Arc::new(Mutex::new(FreeEnergyState::new(100)));
        let agent = StrategyAgent::new(cf, policy, fe);
        assert_eq!(agent.kind(), AgentKind::Strategy);
    }
}
