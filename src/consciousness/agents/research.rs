use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;

use crate::consciousness::bus::BusMessage;
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Priority, Proposal,
    TheoryOfMindBelief, Verdict, VerdictKind,
};
use crate::cosmic::{CosmicMemoryGraph, WorldModel};

pub struct ResearchAgent {
    graph: Arc<Mutex<CosmicMemoryGraph>>,
    world_model: Arc<Mutex<WorldModel>>,
    proposal_counter: u64,
    agent_models: Vec<crate::consciousness::somatic::TheoryOfMind>,
    pending_markers: Vec<crate::consciousness::somatic::SomaticMarker>,
    signal_confidence: HashMap<String, f64>,
}

impl ResearchAgent {
    pub fn new(graph: Arc<Mutex<CosmicMemoryGraph>>, world_model: Arc<Mutex<WorldModel>>) -> Self {
        Self {
            graph,
            world_model,
            proposal_counter: 0,
            agent_models: Vec::new(),
            pending_markers: Vec::new(),
            signal_confidence: HashMap::new(),
        }
    }

    fn next_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter + 2_000_000
    }

    fn topic_for_signal(payload: &str) -> String {
        payload.split(':').next().unwrap_or(payload).to_string()
    }

    fn get_signal_confidence(&self, topic: &str) -> f64 {
        *self.signal_confidence.get(topic).unwrap_or(&0.5)
    }

    fn signal_to_noise(confidence: f64) -> f64 {
        if confidence >= 1.0 {
            return f64::MAX;
        }
        confidence / (1.0 - confidence)
    }

    fn confidence_from_snr(snr: f64) -> f64 {
        if snr <= 1.5 {
            0.3 + (snr / 1.5) * 0.2
        } else if snr >= 3.0 {
            0.8 + ((snr - 3.0) / 7.0).min(0.2)
        } else {
            0.5 + (snr - 1.5) / (3.0 - 1.5) * 0.3
        }
    }
}

impl ConsciousnessAgent for ResearchAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Research
    }

    fn perceive(&mut self, state: &ConsciousnessState, signals: &[BusMessage]) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        self.agent_models.clear();
        for proposal in &state.active_proposals {
            self.agent_models
                .push(crate::consciousness::somatic::TheoryOfMind {
                    agent_id: proposal.source.to_string(),
                    believed_state: proposal.action.clone(),
                    confidence: proposal.confidence,
                    last_updated: Utc::now(),
                });
        }

        let (belief_count, uncertain_empty) = {
            let world = self.world_model.lock();
            (world.belief_count(), world.most_confident(0).is_empty())
        };
        if belief_count > 0 && uncertain_empty {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Research,
                action: "scan_world_model".to_string(),
                reasoning: "WorldModel needs investigation".to_string(),
                confidence: 0.6,
                priority: Priority::Normal,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        for signal in signals {
            if signal.topic == "query" {
                let payload_str = signal.payload.as_str().unwrap_or("unknown");
                let topic = Self::topic_for_signal(payload_str);
                let sig_conf = self.get_signal_confidence(&topic);
                let snr = Self::signal_to_noise(sig_conf);
                let proposal_confidence = Self::confidence_from_snr(snr);

                let priority = if snr > 3.0 {
                    Priority::High
                } else if snr < 1.5 {
                    Priority::Low
                } else {
                    Priority::Normal
                };

                proposals.push(Proposal {
                    id: self.next_id(),
                    source: AgentKind::Research,
                    action: format!("research_query:{}", signal.payload),
                    reasoning: format!("Signal SNR={snr:.2}, confidence={proposal_confidence:.2}"),
                    confidence: proposal_confidence,
                    priority,
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
                voter: AgentKind::Research,
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
            .filter(|p| p.source == AgentKind::Research)
            .map(|p| {
                let (success, learnings) = if p.action == "scan_world_model" {
                    let graph = self.graph.lock();
                    let count = graph.node_count();
                    (
                        count > 0,
                        vec![format!("world_model_scan: {} nodes found", count)],
                    )
                } else if p.action.starts_with("research_query:") {
                    (true, vec![format!("query_resolved: {}", p.action)])
                } else {
                    (true, Vec::new())
                };

                ActionOutcome {
                    agent: AgentKind::Research,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success,
                    impact: if success { 0.6 } else { 0.1 },
                    learnings,
                    timestamp: Utc::now(),
                }
            })
            .collect()
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], state: &ConsciousnessState) {
        self.pending_markers.clear();

        let my_outcomes: Vec<&ActionOutcome> = outcomes
            .iter()
            .filter(|o| o.agent == AgentKind::Research)
            .collect();

        if my_outcomes.is_empty() {
            return;
        }

        let success_rate =
            my_outcomes.iter().filter(|o| o.success).count() as f64 / my_outcomes.len() as f64;

        for model in &mut self.agent_models {
            model.confidence = model.confidence * 0.9 + success_rate * 0.1;
            model.last_updated = Utc::now();
        }

        for outcome in &my_outcomes {
            let topic = Self::topic_for_signal(&outcome.action);
            let observation = if outcome.success { outcome.impact } else { 0.2 };
            let current = self.get_signal_confidence(&topic);
            let updated = current * 0.7 + observation * 0.3;
            self.signal_confidence.insert(topic.clone(), updated);

            for learning in &outcome.learnings {
                let learning_topic = Self::topic_for_signal(learning);
                if learning_topic != topic {
                    let lt_current = self.get_signal_confidence(&learning_topic);
                    let boosted = (lt_current + 0.05).min(1.0);
                    self.signal_confidence.insert(learning_topic, boosted);
                }
            }

            if outcome.impact > 0.7 {
                self.pending_markers
                    .push(crate::consciousness::somatic::SomaticMarker {
                        marker_type: if outcome.success {
                            "research_discovery".to_string()
                        } else {
                            "research_dead_end".to_string()
                        },
                        intensity: outcome.impact,
                        trigger: outcome.action.clone(),
                        timestamp: Utc::now(),
                    });
            }
        }

        if success_rate < 0.5 && state.coherence < 0.7 {
            self.agent_models.clear();
        }
    }

    fn somatic_markers(&self) -> Vec<crate::consciousness::somatic::SomaticMarker> {
        self.pending_markers.clone()
    }

    fn theory_of_mind_beliefs(&self) -> Vec<TheoryOfMindBelief> {
        self.agent_models
            .iter()
            .map(|model| {
                let about_agent = match model.agent_id.as_str() {
                    "Chairman" => AgentKind::Chairman,
                    "Memory" => AgentKind::Memory,
                    "Strategy" => AgentKind::Strategy,
                    "Execution" => AgentKind::Execution,
                    "Conscience" => AgentKind::Conscience,
                    "Reflection" => AgentKind::Reflection,
                    "Metacognitive" => AgentKind::Metacognitive,
                    _ => AgentKind::Research,
                };
                TheoryOfMindBelief {
                    about_agent,
                    belief: model.believed_state.clone(),
                    confidence: model.confidence,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn research_agent_kind() {
        let graph = Arc::new(Mutex::new(CosmicMemoryGraph::new(1000)));
        let wm = Arc::new(Mutex::new(WorldModel::new(100)));
        let agent = ResearchAgent::new(graph, wm);
        assert_eq!(agent.kind(), AgentKind::Research);
    }

    #[test]
    fn act_returns_nonempty_learnings() {
        let graph = Arc::new(Mutex::new(CosmicMemoryGraph::new(1000)));
        let wm = Arc::new(Mutex::new(WorldModel::new(100)));
        let mut agent = ResearchAgent::new(graph, wm);

        let proposals = vec![
            Proposal {
                id: 1,
                source: AgentKind::Research,
                action: "scan_world_model".to_string(),
                reasoning: "test".to_string(),
                confidence: 0.6,
                priority: Priority::Normal,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            },
            Proposal {
                id: 2,
                source: AgentKind::Research,
                action: "research_query:test_topic".to_string(),
                reasoning: "test".to_string(),
                confidence: 0.5,
                priority: Priority::Normal,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            },
        ];

        let outcomes = agent.act(&proposals);
        assert_eq!(outcomes.len(), 2);
        for outcome in &outcomes {
            assert!(
                !outcome.learnings.is_empty(),
                "learnings should not be empty for action: {}",
                outcome.action
            );
        }
        assert!(outcomes[0].learnings[0].contains("world_model_scan"));
        assert!(outcomes[1].learnings[0].contains("query_resolved"));
    }
}
