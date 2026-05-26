use parking_lot::Mutex;
use std::sync::Arc;

use chrono::Utc;

use crate::consciousness::bus::BusMessage;
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Priority, Proposal, Verdict,
    VerdictKind,
};
use crate::cosmic::{ConsolidationEngine, CosmicMemoryGraph};

pub struct MemoryAgent {
    graph: Arc<Mutex<CosmicMemoryGraph>>,
    consolidation: Arc<Mutex<ConsolidationEngine>>,
    proposal_counter: u64,
    episodes: Vec<crate::consciousness::somatic::AutobiographicalMemory>,
}

impl MemoryAgent {
    pub fn new(
        graph: Arc<Mutex<CosmicMemoryGraph>>,
        consolidation: Arc<Mutex<ConsolidationEngine>>,
    ) -> Self {
        Self {
            graph,
            consolidation,
            proposal_counter: 0,
            episodes: Vec::new(),
        }
    }

    fn next_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter + 1_000_000
    }
}

impl ConsciousnessAgent for MemoryAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Memory
    }

    fn perceive(&mut self, _state: &ConsciousnessState, signals: &[BusMessage]) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        let entry_count = self.consolidation.lock().entry_count();
        if entry_count > 0 {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Memory,
                action: "consolidate_memories".to_string(),
                reasoning: format!("Consolidation engine has {} entries pending", entry_count),
                confidence: 0.8,
                priority: Priority::Normal,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        for signal in signals {
            if signal.topic == "store_insight" {
                proposals.push(Proposal {
                    id: self.next_id(),
                    source: AgentKind::Memory,
                    action: format!("store:{}", signal.payload),
                    reasoning: "Received insight for storage".to_string(),
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
                voter: AgentKind::Memory,
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
            .filter(|p| p.source == AgentKind::Memory)
            .map(|p| {
                let success = if p.action == "consolidate_memories" {
                    let mut consolidation = self.consolidation.lock();
                    let result = consolidation.consolidate();
                    result.patterns_found > 0 || result.pruned_count > 0
                } else {
                    true
                };

                ActionOutcome {
                    agent: AgentKind::Memory,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success,
                    impact: if success { 0.5 } else { 0.0 },
                    learnings: Vec::new(),
                    timestamp: Utc::now(),
                }
            })
            .collect()
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], state: &ConsciousnessState) {
        self.episodes.clear();
        for outcome in outcomes {
            if outcome.impact > 0.5 {
                self.episodes
                    .push(crate::consciousness::somatic::AutobiographicalMemory {
                        episode_id: state.tick_count * 1000 + outcome.proposal_id,
                        context: outcome.action.clone(),
                        outcome: if outcome.success {
                            "success".to_string()
                        } else {
                            "failure".to_string()
                        },
                        emotional_valence: if outcome.success {
                            outcome.impact
                        } else {
                            -outcome.impact
                        },
                        tick: state.tick_count,
                    });
            }
        }
    }

    fn autobiographical_episodes(
        &self,
    ) -> Vec<crate::consciousness::somatic::AutobiographicalMemory> {
        self.episodes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_memory_agent() -> MemoryAgent {
        let graph = Arc::new(Mutex::new(CosmicMemoryGraph::new(1000)));
        let consolidation = Arc::new(Mutex::new(ConsolidationEngine::new(0.8)));
        MemoryAgent::new(graph, consolidation)
    }

    #[test]
    fn perceive_checks_consolidation() {
        let mut agent = make_memory_agent();
        let state = ConsciousnessState::default();
        let proposals = agent.perceive(&state, &[]);
        assert!(proposals.iter().all(|p| p.source == AgentKind::Memory));
    }
}
