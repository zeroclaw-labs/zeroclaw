use parking_lot::Mutex;
use std::sync::Arc;

use chrono::Utc;

use crate::consciousness::bus::BusMessage;
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Priority, Proposal, Verdict,
    VerdictKind,
};
use crate::cosmic::{CausalGraph, EmotionalModulator, GlobalVariable};

pub struct ExecutionAgent {
    causal: Arc<Mutex<CausalGraph>>,
    modulator: Arc<Mutex<EmotionalModulator>>,
    proposal_counter: u64,
    pending_markers: Vec<crate::consciousness::somatic::SomaticMarker>,
}

impl ExecutionAgent {
    pub fn new(causal: Arc<Mutex<CausalGraph>>, modulator: Arc<Mutex<EmotionalModulator>>) -> Self {
        Self {
            causal,
            modulator,
            proposal_counter: 0,
            pending_markers: Vec::new(),
        }
    }

    fn next_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter + 4_000_000
    }
}

impl ConsciousnessAgent for ExecutionAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Execution
    }

    fn perceive(&mut self, _state: &ConsciousnessState, signals: &[BusMessage]) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        let arousal = self.modulator.lock().get_variable(GlobalVariable::Arousal);
        if arousal > 0.8 {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Execution,
                action: "dampen_arousal".to_string(),
                reasoning: format!("Arousal {arousal:.2} too high for reliable execution"),
                confidence: 0.8,
                priority: Priority::High,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        for signal in signals {
            if signal.topic == "execute_action" {
                proposals.push(Proposal {
                    id: self.next_id(),
                    source: AgentKind::Execution,
                    action: format!("run:{}", signal.payload),
                    reasoning: "Execution request received".to_string(),
                    confidence: 0.7,
                    priority: Priority::Normal,
                    contradicts: Vec::new(),
                    timestamp: Utc::now(),
                });
            }
        }

        proposals
    }

    fn deliberate(&mut self, proposals: &[Proposal], _state: &ConsciousnessState) -> Vec<Verdict> {
        proposals
            .iter()
            .map(|p| Verdict {
                voter: AgentKind::Execution,
                proposal_id: p.id,
                kind: VerdictKind::Approve,
                confidence: 0.5,
                objection: None,
            })
            .collect()
    }

    fn act(&mut self, approved: &[Proposal]) -> Vec<ActionOutcome> {
        approved
            .iter()
            .filter(|p| p.source == AgentKind::Execution)
            .map(|p| {
                let success = if p.action == "dampen_arousal" {
                    self.modulator
                        .lock()
                        .nudge_variable(GlobalVariable::Arousal, -0.2);
                    true
                } else if p.action.starts_with("run:") {
                    let mut causal = self.causal.lock();
                    causal.record_event("consciousness_execution", &p.action, 1.0, 0.0, 0);
                    true
                } else {
                    true
                };

                ActionOutcome {
                    agent: AgentKind::Execution,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success,
                    impact: if success { 0.6 } else { 0.0 },
                    learnings: Vec::new(),
                    timestamp: Utc::now(),
                }
            })
            .collect()
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], _state: &ConsciousnessState) {
        self.pending_markers.clear();
        for outcome in outcomes {
            if outcome.success {
                self.causal.lock().record_event(
                    &format!("completed:{}", outcome.action),
                    "reflection",
                    1.0,
                    0.0,
                    0,
                );
            }
            if outcome.impact > 0.7 {
                self.pending_markers
                    .push(crate::consciousness::somatic::SomaticMarker {
                        marker_type: if outcome.success {
                            "positive_execution".to_string()
                        } else {
                            "negative_execution".to_string()
                        },
                        intensity: outcome.impact,
                        trigger: outcome.action.clone(),
                        timestamp: Utc::now(),
                    });
            }
        }
    }

    fn somatic_markers(&self) -> Vec<crate::consciousness::somatic::SomaticMarker> {
        self.pending_markers.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_agent_kind() {
        let causal = Arc::new(Mutex::new(CausalGraph::new(100)));
        let modulator = Arc::new(Mutex::new(EmotionalModulator::new()));
        let agent = ExecutionAgent::new(causal, modulator);
        assert_eq!(agent.kind(), AgentKind::Execution);
    }
}
