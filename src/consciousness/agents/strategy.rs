use parking_lot::Mutex;
use std::collections::HashMap;
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
    priors: HashMap<String, f64>,
    min_edge: f64,
    ticks_since_prior_update: HashMap<String, u64>,
    pending_tom_beliefs: Vec<crate::consciousness::traits::TheoryOfMindBelief>,
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
            priors: HashMap::new(),
            min_edge: 0.05,
            ticks_since_prior_update: HashMap::new(),
            pending_tom_beliefs: Vec::new(),
        }
    }

    pub fn with_min_edge(mut self, min_edge: f64) -> Self {
        self.min_edge = min_edge;
        self
    }

    fn domain_for_action(action: &str) -> String {
        action.split(':').next().unwrap_or(action).to_string()
    }

    fn get_prior(&self, domain: &str) -> f64 {
        *self.priors.get(domain).unwrap_or(&0.5)
    }

    fn update_posterior(&mut self, domain: &str, observation: f64) {
        let prior = self.get_prior(domain);
        let posterior = prior * 0.8 + observation * 0.2;
        self.priors.insert(domain.to_string(), posterior);
        self.ticks_since_prior_update.insert(domain.to_string(), 0);
    }

    fn decay_stale_priors(&mut self) {
        for (domain, ticks) in &mut self.ticks_since_prior_update {
            *ticks += 1;
            if *ticks > 10 {
                if let Some(prior) = self.priors.get_mut(domain) {
                    *prior = *prior * 0.95 + 0.5 * 0.05;
                }
            }
        }
    }

    fn compute_edge(&self, domain: &str, proposal_confidence: f64) -> f64 {
        let posterior = self.get_prior(domain);
        posterior - proposal_confidence
    }

    fn kelly_fraction(edge: f64, posterior: f64) -> f64 {
        if posterior >= 1.0 || edge <= 0.0 {
            return 0.0;
        }
        (edge / (1.0 - posterior)).clamp(0.0, 0.25)
    }

    fn wisdom_boost(state: &ConsciousnessState, action: &str) -> f64 {
        let mut boost = 0.0_f64;
        for entry in &state.wisdom_entries {
            if entry.confidence >= 0.5 && action.contains(&entry.domain) {
                boost += entry.confidence * 0.15;
            }
        }
        boost.min(0.3)
    }

    fn somatic_risk_modifier(state: &ConsciousnessState) -> f64 {
        let max_urgency = state
            .homeostatic_drives
            .iter()
            .map(|d| d.urgency)
            .fold(0.0_f64, f64::max);
        let stress_markers = state
            .somatic_markers
            .iter()
            .filter(|m| m.marker_type == "stress" || m.marker_type == "danger")
            .map(|m| m.intensity)
            .sum::<f64>();
        (max_urgency * 0.3 + stress_markers * 0.2).min(0.5)
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

    fn perceive(&mut self, state: &ConsciousnessState, _signals: &[BusMessage]) -> Vec<Proposal> {
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

        for entry in &state.wisdom_entries {
            if entry.confidence >= 0.7 {
                proposals.push(Proposal {
                    id: self.next_id(),
                    source: AgentKind::Strategy,
                    action: format!("wisdom_guided:{}", entry.principle),
                    reasoning: format!(
                        "High-confidence wisdom ({:.2}) in domain '{}' guides action",
                        entry.confidence, entry.domain
                    ),
                    confidence: entry.confidence,
                    priority: Priority::Normal,
                    contradicts: Vec::new(),
                    timestamp: Utc::now(),
                });
            }
        }

        proposals
    }

    fn deliberate(&mut self, proposals: &[Proposal], state: &ConsciousnessState) -> Vec<Verdict> {
        self.decay_stale_priors();
        let somatic_penalty = Self::somatic_risk_modifier(state);

        proposals
            .iter()
            .map(|p| {
                let mut policy = self.policy.lock();
                let decision = policy.evaluate(&p.action, "consciousness");

                let domain = Self::domain_for_action(&p.action);
                let raw_edge = self.compute_edge(&domain, p.confidence);
                let friction = 1.0 - state.coherence;
                let wisdom_boost = Self::wisdom_boost(state, &p.action);
                let neuro = &state.neuromodulation;
                let dopamine_boost = (neuro.dopamine - 0.5) * 0.1;
                let cortisol_drag = neuro.cortisol * 0.15;
                let net_edge = raw_edge - friction * 0.1 + wisdom_boost - somatic_penalty + dopamine_boost - cortisol_drag;

                let cf_risk = {
                    let mut cf = self.counterfactual.lock();
                    let scenario = Scenario {
                        id: format!("delib_{}", p.id),
                        action: p.action.clone(),
                        context: std::collections::HashMap::new(),
                        created_at: Utc::now(),
                    };
                    let result = cf.simulate(&scenario);
                    if result.confidence > 0.0 {
                        1.0 - result.confidence
                    } else {
                        0.0
                    }
                };
                let adjusted_edge = net_edge - cf_risk * 0.2;

                let calibration_factor = state
                    .agent_calibration
                    .iter()
                    .find(|c| c.agent == p.source)
                    .map(|c| {
                        if c.total_predictions >= 5 {
                            (1.0 - c.calibration_error).max(0.3)
                        } else {
                            1.0
                        }
                    })
                    .unwrap_or(1.0);

                let (kind, confidence) = if decision.score < 0.0 {
                    (VerdictKind::Reject, (decision.score.abs()).min(1.0))
                } else if adjusted_edge >= self.min_edge {
                    let posterior = self.get_prior(&domain);
                    let kelly = Self::kelly_fraction(adjusted_edge, posterior);
                    let base_conf = (decision.score.abs()).min(1.0);
                    (VerdictKind::Approve, (base_conf * (0.5 + kelly * 2.0) * calibration_factor + wisdom_boost).min(1.0))
                } else {
                    (
                        VerdictKind::Reject,
                        0.3,
                    )
                };

                Verdict {
                    voter: AgentKind::Strategy,
                    proposal_id: p.id,
                    kind,
                    confidence,
                    objection: if kind == VerdictKind::Reject {
                        Some(format!(
                            "edge={adjusted_edge:.3} min={:.3} (policy={:.2}, wisdom={wisdom_boost:.2}, somatic={somatic_penalty:.2}, cf_risk={cf_risk:.2})",
                            self.min_edge, decision.score
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
            let domain = Self::domain_for_action(&outcome.action);
            let observation = if outcome.success { outcome.impact } else { 0.1 };
            self.update_posterior(&domain, observation);

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

        self.pending_tom_beliefs.clear();
        for cal in &state.agent_calibration {
            if cal.total_predictions >= 3 {
                let belief = if cal.calibration_error < 0.2 {
                    format!("{:?} is well-calibrated (error={:.2})", cal.agent, cal.calibration_error)
                } else {
                    format!("{:?} is poorly calibrated (error={:.2}), discount its predictions", cal.agent, cal.calibration_error)
                };
                self.pending_tom_beliefs.push(
                    crate::consciousness::traits::TheoryOfMindBelief {
                        about_agent: cal.agent,
                        belief,
                        confidence: (1.0 - cal.calibration_error).max(0.1),
                    },
                );
            }
        }
    }

    fn theory_of_mind_beliefs(&self) -> Vec<crate::consciousness::traits::TheoryOfMindBelief> {
        self.pending_tom_beliefs.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent() -> StrategyAgent {
        let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 10)));
        let policy = Arc::new(Mutex::new(PolicyEngine::new(10)));
        let fe = Arc::new(Mutex::new(FreeEnergyState::new(100)));
        StrategyAgent::new(cf, policy, fe)
    }

    #[test]
    fn strategy_agent_kind() {
        let agent = make_agent();
        assert_eq!(agent.kind(), AgentKind::Strategy);
    }

    #[test]
    fn bayesian_edge_approval() {
        let mut agent = make_agent().with_min_edge(0.05);

        agent.priors.insert("research_query".to_string(), 0.8);

        let proposal = Proposal {
            id: 1,
            source: AgentKind::Research,
            action: "research_query:test".to_string(),
            reasoning: "test".to_string(),
            confidence: 0.5,
            priority: Priority::Normal,
            contradicts: Vec::new(),
            timestamp: Utc::now(),
        };

        let state = ConsciousnessState {
            coherence: 0.9,
            ..Default::default()
        };

        let verdicts = agent.deliberate(&[proposal], &state);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].kind, VerdictKind::Approve);
    }

    #[test]
    fn low_edge_rejected() {
        let mut agent = make_agent().with_min_edge(0.1);

        agent.priors.insert("scan".to_string(), 0.5);

        let proposal = Proposal {
            id: 1,
            source: AgentKind::Strategy,
            action: "scan:world".to_string(),
            reasoning: "test".to_string(),
            confidence: 0.5,
            priority: Priority::Normal,
            contradicts: Vec::new(),
            timestamp: Utc::now(),
        };

        let state = ConsciousnessState {
            coherence: 0.9,
            ..Default::default()
        };

        let verdicts = agent.deliberate(&[proposal], &state);
        assert_eq!(verdicts[0].kind, VerdictKind::Reject);
    }

    #[test]
    fn kelly_fraction_clamped() {
        assert!(StrategyAgent::kelly_fraction(0.5, 0.3) <= 0.25);
        assert!(StrategyAgent::kelly_fraction(-0.1, 0.5) == 0.0);
        assert!(StrategyAgent::kelly_fraction(0.1, 1.0) == 0.0);
    }

    #[test]
    fn prior_decay_toward_neutral() {
        let mut agent = make_agent();
        agent.priors.insert("test".to_string(), 0.9);
        agent
            .ticks_since_prior_update
            .insert("test".to_string(), 15);
        agent.decay_stale_priors();
        let prior = agent.get_prior("test");
        assert!(prior < 0.9, "prior should decay: {prior}");
        assert!(prior > 0.5, "prior should not overshoot neutral: {prior}");
    }

    #[test]
    fn reflect_updates_posteriors() {
        let mut agent = make_agent();

        let outcomes = vec![ActionOutcome {
            agent: AgentKind::Strategy,
            proposal_id: 1,
            action: "pursue_goal:test".to_string(),
            success: true,
            impact: 0.8,
            learnings: Vec::new(),
            timestamp: Utc::now(),
        }];

        let state = ConsciousnessState::default();
        agent.reflect(&outcomes, &state);

        let posterior = agent.get_prior("pursue_goal");
        assert!(
            posterior > 0.5,
            "posterior should increase after success: {posterior}"
        );
    }
}
