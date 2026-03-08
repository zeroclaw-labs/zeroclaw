use chrono::Utc;

use crate::consciousness::bus::BusMessage;
use crate::consciousness::metacognition::{MetacognitiveEngine, MetacognitivePolicy};
use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, PhenomenalState, Priority,
    Proposal, Verdict, VerdictKind,
};

pub struct MetacognitiveAgent {
    engine: MetacognitiveEngine,
    proposal_counter: u64,
    phenomenal: PhenomenalState,
    adjustment_success_count: u64,
    adjustment_total_count: u64,
}

impl MetacognitiveAgent {
    pub fn new(policy: MetacognitivePolicy) -> Self {
        Self {
            engine: MetacognitiveEngine::new(policy),
            proposal_counter: 0,
            phenomenal: PhenomenalState::default(),
            adjustment_success_count: 0,
            adjustment_total_count: 0,
        }
    }

    fn next_id(&mut self) -> u64 {
        self.proposal_counter += 1;
        self.proposal_counter + 7_000_000
    }
}

impl ConsciousnessAgent for MetacognitiveAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Metacognitive
    }

    fn perceive(&mut self, state: &ConsciousnessState, _signals: &[BusMessage]) -> Vec<Proposal> {
        let mut proposals = Vec::new();

        let generated = state.active_proposals.len();
        let approved = state.recent_outcomes.iter().filter(|o| o.success).count();
        let vetoed = generated.saturating_sub(approved);
        self.engine.observe(state, generated, approved, vetoed, 0);

        if state.coherence < 0.5 {
            proposals.push(Proposal {
                id: self.next_id(),
                source: AgentKind::Metacognitive,
                action: "adjust:coherence_ema_alpha".to_string(),
                reasoning: format!(
                    "Coherence {:.2} below threshold — smoothing EMA for stability",
                    state.coherence
                ),
                confidence: 0.7,
                priority: Priority::High,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            });
        }

        if let Some(trend) = self.engine.recent_coherence_trend() {
            if trend < -0.1 {
                proposals.push(Proposal {
                    id: self.next_id(),
                    source: AgentKind::Metacognitive,
                    action: "adjust:debate_rounds".to_string(),
                    reasoning: format!(
                        "Coherence trend {trend:.3} declining — consider adjusting debate rounds"
                    ),
                    confidence: 0.6,
                    priority: Priority::Normal,
                    contradicts: Vec::new(),
                    timestamp: Utc::now(),
                });
            }
        }

        if let Some(rate) = self.engine.approval_rate() {
            if !(0.2..=0.95).contains(&rate) {
                proposals.push(Proposal {
                    id: self.next_id(),
                    source: AgentKind::Metacognitive,
                    action: "adjust:approval_threshold".to_string(),
                    reasoning: format!(
                        "Approval rate {rate:.2} is skewed — threshold adjustment needed"
                    ),
                    confidence: 0.65,
                    priority: Priority::Normal,
                    contradicts: Vec::new(),
                    timestamp: Utc::now(),
                });
            }
        }

        proposals
    }

    fn deliberate(&mut self, proposals: &[Proposal], state: &ConsciousnessState) -> Vec<Verdict> {
        proposals
            .iter()
            .map(|p| {
                let recent_success_rate = if state.recent_outcomes.is_empty() {
                    0.5
                } else {
                    let source_outcomes: Vec<&ActionOutcome> = state
                        .recent_outcomes
                        .iter()
                        .filter(|o| o.agent == p.source)
                        .collect();
                    if source_outcomes.is_empty() {
                        0.5
                    } else {
                        source_outcomes.iter().filter(|o| o.success).count() as f64
                            / source_outcomes.len() as f64
                    }
                };

                let stable_enough = state.coherence >= 0.4;
                let source_reliable = recent_success_rate >= 0.4;

                let kind =
                    if (stable_enough && source_reliable)
                        || (!stable_enough && p.action.starts_with("adjust:"))
                    {
                        VerdictKind::Approve
                    } else {
                        VerdictKind::Reject
                    };

                Verdict {
                    voter: AgentKind::Metacognitive,
                    proposal_id: p.id,
                    kind,
                    confidence: 0.4,
                    objection: if kind == VerdictKind::Reject {
                        Some(format!(
                            "System unstable (coherence={:.2}) or source unreliable (success_rate={:.2})",
                            state.coherence, recent_success_rate
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
            .filter(|p| p.source == AgentKind::Metacognitive)
            .map(|p| {
                let learning = format!("Metacognitive adjustment proposed: {}", p.action);
                ActionOutcome {
                    agent: AgentKind::Metacognitive,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success: true,
                    impact: 0.4,
                    learnings: vec![learning],
                    timestamp: Utc::now(),
                }
            })
            .collect()
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], _state: &ConsciousnessState) {
        for outcome in outcomes
            .iter()
            .filter(|o| o.agent == AgentKind::Metacognitive)
        {
            self.adjustment_total_count += 1;
            if outcome.success {
                self.adjustment_success_count += 1;
            }
        }
    }

    fn vote_weight(&self) -> f64 {
        0.3
    }

    fn phenomenal_state(&self) -> PhenomenalState {
        self.phenomenal
    }

    fn update_phenomenal(&mut self, _outcomes: &[ActionOutcome], state: &ConsciousnessState) {
        let trend = self.engine.recent_coherence_trend().unwrap_or(0.0);
        let volatility = trend.abs();
        self.phenomenal.attention = (0.5 + volatility * 2.0).min(1.0);

        let freq = if self.adjustment_total_count > 0 {
            (self.adjustment_total_count as f64 / 100.0).min(1.0)
        } else {
            0.1
        };
        self.phenomenal.arousal = freq;

        self.phenomenal.valence = if state.coherence >= 0.7 {
            0.5 + trend.max(0.0)
        } else {
            -0.5 + trend.min(0.0)
        }
        .clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(coherence: f64) -> ConsciousnessState {
        ConsciousnessState {
            coherence,
            tick_count: 1,
            ..Default::default()
        }
    }

    #[test]
    fn metacognitive_agent_kind() {
        let agent = MetacognitiveAgent::new(MetacognitivePolicy::default());
        assert_eq!(agent.kind(), AgentKind::Metacognitive);
    }

    #[test]
    fn perceive_generates_proposals_on_low_coherence() {
        let mut agent = MetacognitiveAgent::new(MetacognitivePolicy::default());
        let state = make_state(0.3);
        let proposals = agent.perceive(&state, &[]);
        assert!(proposals
            .iter()
            .any(|p| p.action.contains("coherence_ema_alpha")));
    }

    #[test]
    fn low_vote_weight() {
        let agent = MetacognitiveAgent::new(MetacognitivePolicy::default());
        assert!((agent.vote_weight() - 0.3).abs() < f64::EPSILON);
    }
}
