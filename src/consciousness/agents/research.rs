use parking_lot::Mutex;
use std::sync::Arc;

use chrono::Utc;

use crate::consciousness::bus::BusMessage;
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Priority, Proposal, Verdict,
    VerdictKind,
};
use crate::cosmic::{CosmicMemoryGraph, WorldModel};

pub struct ResearchAgent {
    graph: Arc<Mutex<CosmicMemoryGraph>>,
    world_model: Arc<Mutex<WorldModel>>,
    proposal_counter: u64,
    agent_models: Vec<crate::consciousness::somatic::TheoryOfMind>,
    pending_markers: Vec<crate::consciousness::somatic::SomaticMarker>,
}

impl ResearchAgent {
    pub fn new(graph: Arc<Mutex<CosmicMemoryGraph>>, world_model: Arc<Mutex<WorldModel>>) -> Self {
        Self {
            graph,
            world_model,
            proposal_counter: 0,
            agent_models: Vec::new(),
            pending_markers: Vec::new(),
        }
    }

    fn next_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter + 2_000_000
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
                proposals.push(Proposal {
                    id: self.next_id(),
                    source: AgentKind::Research,
                    action: format!("research_query:{}", signal.payload),
                    reasoning: "Research query received".to_string(),
                    confidence: 0.7,
                    priority: Priority::High,
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
                let success = if p.action == "scan_world_model" {
                    let graph = self.graph.lock();
                    graph.node_count() > 0
                } else {
                    true
                };

                ActionOutcome {
                    agent: AgentKind::Research,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success,
                    impact: if success { 0.6 } else { 0.1 },
                    learnings: Vec::new(),
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
}
