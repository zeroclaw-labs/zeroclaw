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

            if signal.topic == "dream_pattern" {
                if let Some(pattern) = signal.payload.get("pattern").and_then(|v| v.as_str()) {
                    let mut causal = self.causal.lock();
                    causal.record_event("dream_consolidation", pattern, 0.8, 0.0, 0);
                }
            }
        }

        proposals
    }

    fn deliberate(&mut self, proposals: &[Proposal], state: &ConsciousnessState) -> Vec<Verdict> {
        let stress_level: f64 = state
            .somatic_markers
            .iter()
            .filter(|m| {
                m.marker_type == "stress"
                    || m.marker_type == "danger"
                    || m.marker_type == "negative_execution"
            })
            .map(|m| m.intensity)
            .sum::<f64>()
            .min(1.0);

        let neuro_stress = stress_level + state.neuromodulation.cortisol * 0.3
            - state.neuromodulation.dopamine * 0.1;
        let throttle_threshold = (0.7 - state.neuromodulation.serotonin * 0.1).max(0.4);

        proposals
            .iter()
            .map(|p| {
                let throttled = neuro_stress > throttle_threshold && p.priority != Priority::Critical;
                if throttled {
                    Verdict {
                        voter: AgentKind::Execution,
                        proposal_id: p.id,
                        kind: VerdictKind::Reject,
                        confidence: 0.6,
                        objection: Some(format!(
                            "Execution throttled: neuro_stress={neuro_stress:.2} > threshold={throttle_threshold:.2}"
                        )),
                    }
                } else {
                    Verdict {
                        voter: AgentKind::Execution,
                        proposal_id: p.id,
                        kind: VerdictKind::Approve,
                        confidence: (0.5 + (1.0 - stress_level) * 0.3).min(1.0),
                        objection: None,
                    }
                }
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
