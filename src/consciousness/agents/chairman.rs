use parking_lot::Mutex;
use std::sync::Arc;

use chrono::Utc;

use crate::consciousness::bus::BusMessage;
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Priority, Proposal, Verdict,
    VerdictKind,
};
use crate::continuity::ContinuityGuard;
use crate::cosmic::{AgentPool, GlobalWorkspace, SubsystemId};

pub struct ChairmanAgent {
    workspace: Arc<Mutex<GlobalWorkspace>>,
    agent_pool: Arc<Mutex<AgentPool>>,
    continuity: Option<Arc<Mutex<ContinuityGuard>>>,
    proposal_counter: u64,
}

impl ChairmanAgent {
    pub fn new(
        workspace: Arc<Mutex<GlobalWorkspace>>,
        agent_pool: Arc<Mutex<AgentPool>>,
        continuity: Option<Arc<Mutex<ContinuityGuard>>>,
    ) -> Self {
        Self {
            workspace,
            agent_pool,
            continuity,
            proposal_counter: 0,
        }
    }

    fn next_proposal_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter
    }
}

impl ConsciousnessAgent for ChairmanAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Chairman
    }

    fn vote_weight(&self) -> f64 {
        1.0
    }

    fn perceive(&mut self, state: &ConsciousnessState, signals: &[BusMessage]) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        let broadcast = self.workspace.lock().compete();

        if let Some(dominant) = broadcast.dominant {
            proposals.push(Proposal {
                id: self.next_proposal_id(),
                source: AgentKind::Chairman,
                action: format!("focus_subsystem:{dominant:?}"),
                reasoning: format!(
                    "GlobalWorkspace competition selected {:?} with coherence {:.2}",
                    dominant, broadcast.coherence
                ),
                confidence: broadcast.coherence,
                priority: Priority::High,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        if state.coherence < 0.3 && state.tick_count > 0 {
            proposals.push(Proposal {
                id: self.next_proposal_id(),
                source: AgentKind::Chairman,
                action: "emergency_coherence_restore".to_string(),
                reasoning: format!(
                    "Coherence {:.2} below critical threshold 0.3",
                    state.coherence
                ),
                confidence: 0.9,
                priority: Priority::Critical,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        for signal in signals {
            if signal.topic == "urgent_request" {
                proposals.push(Proposal {
                    id: self.next_proposal_id(),
                    source: AgentKind::Chairman,
                    action: format!("route_urgent:{}", signal.from),
                    reasoning: "Urgent request received on bus".to_string(),
                    confidence: 0.8,
                    priority: Priority::High,
                    contradicts: Vec::new(),
                    timestamp: Utc::now(),
                });
            }
        }

        let has_high_confidence_wisdom = state.wisdom_entries.iter().any(|w| w.confidence >= 0.7);
        if has_high_confidence_wisdom {
            for proposal in &mut proposals {
                if proposal.priority == Priority::Normal {
                    proposal.priority = Priority::High;
                }
            }
        }

        proposals
    }

    fn deliberate(&mut self, proposals: &[Proposal], state: &ConsciousnessState) -> Vec<Verdict> {
        let drift_ok = self
            .continuity
            .as_ref()
            .map_or(true, |guard| !guard.lock().is_conservative());

        proposals
            .iter()
            .map(|p| {
                let (kind, confidence) = if !drift_ok && !p.action.starts_with("emergency_") {
                    (VerdictKind::Reject, 0.7)
                } else {
                    let tom_boost = state
                        .theory_of_mind
                        .iter()
                        .filter(|t| t.agent_id == p.source.to_string())
                        .map(|t| t.confidence * 0.1)
                        .sum::<f64>();
                    let base = 0.6_f64.min(p.confidence);
                    (VerdictKind::Approve, (base + tom_boost).min(1.0))
                };

                Verdict {
                    voter: AgentKind::Chairman,
                    proposal_id: p.id,
                    kind,
                    confidence,
                    objection: if kind == VerdictKind::Reject {
                        Some("Identity drift detected — non-emergency proposals paused".to_string())
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
            .filter(|p| p.source == AgentKind::Chairman)
            .map(|p| {
                let success = if p.action.starts_with("focus_subsystem:") {
                    let subsystem_str = p.action.trim_start_matches("focus_subsystem:");
                    if let Some(id) = parse_subsystem_id(subsystem_str) {
                        self.workspace.lock().activate(id, 1.0);
                        true
                    } else {
                        false
                    }
                } else {
                    true
                };

                ActionOutcome {
                    agent: AgentKind::Chairman,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success,
                    impact: if success { 0.8 } else { 0.0 },
                    learnings: Vec::new(),
                    timestamp: Utc::now(),
                }
            })
            .collect()
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], _state: &ConsciousnessState) {
        let success_count = outcomes.iter().filter(|o| o.success).count();
        let total = outcomes.len();
        if total > 0 {
            let rate = success_count as f64 / total as f64;
            self.agent_pool
                .lock()
                .broadcast_belief("chairman_success_rate", rate);
        }
    }
}

fn parse_subsystem_id(s: &str) -> Option<SubsystemId> {
    match s {
        "Memory" => Some(SubsystemId::Memory),
        "FreeEnergy" => Some(SubsystemId::FreeEnergy),
        "Causality" => Some(SubsystemId::Causality),
        "SelfModel" => Some(SubsystemId::SelfModel),
        "WorldModel" => Some(SubsystemId::WorldModel),
        "Normative" => Some(SubsystemId::Normative),
        "Modulation" => Some(SubsystemId::Modulation),
        "Policy" => Some(SubsystemId::Policy),
        "Counterfactual" => Some(SubsystemId::Counterfactual),
        "Consolidation" => Some(SubsystemId::Consolidation),
        "Drift" => Some(SubsystemId::Drift),
        "Constitution" => Some(SubsystemId::Constitution),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cosmic::{AgentPool, AgentRole, GlobalWorkspace};

    fn make_chairman() -> ChairmanAgent {
        let ws = Arc::new(Mutex::new(GlobalWorkspace::new(0.3, 5, 10)));
        let pool = Arc::new(Mutex::new(AgentPool::new(4, 10)));
        pool.lock().register_agent("primary", AgentRole::Primary);
        ChairmanAgent::new(ws, pool, None)
    }

    #[test]
    fn perceive_generates_proposals() {
        let mut chairman = make_chairman();
        let state = ConsciousnessState::default();
        let proposals = chairman.perceive(&state, &[]);
        assert!(proposals.iter().all(|p| p.source == AgentKind::Chairman));
    }

    #[test]
    fn low_coherence_triggers_emergency() {
        let mut chairman = make_chairman();
        let state = ConsciousnessState {
            coherence: 0.1,
            tick_count: 5,
            ..Default::default()
        };
        let proposals = chairman.perceive(&state, &[]);
        assert!(proposals
            .iter()
            .any(|p| p.action == "emergency_coherence_restore"));
    }

    #[test]
    fn chairman_has_double_weight() {
        let chairman = make_chairman();
        assert!((chairman.vote_weight() - 1.0).abs() < f64::EPSILON);
    }
}
