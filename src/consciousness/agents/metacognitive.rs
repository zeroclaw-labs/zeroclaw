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
    calibration_drift: f64,
    consecutive_failures: u64,
    kill_switch_active: bool,
    clear_ticks: u64,
    calibration_drift_threshold: f64,
    kill_switch_recovery_ticks: u64,
}

impl MetacognitiveAgent {
    pub fn new(policy: MetacognitivePolicy) -> Self {
        Self {
            engine: MetacognitiveEngine::new(policy),
            proposal_counter: 0,
            phenomenal: PhenomenalState::default(),
            adjustment_success_count: 0,
            adjustment_total_count: 0,
            calibration_drift: 0.0,
            consecutive_failures: 0,
            kill_switch_active: false,
            clear_ticks: 0,
            calibration_drift_threshold: 0.3,
            kill_switch_recovery_ticks: 3,
        }
    }

    pub fn with_thresholds(mut self, drift: f64, recovery: u64) -> Self {
        self.calibration_drift_threshold = drift;
        self.kill_switch_recovery_ticks = recovery;
        self
    }

    pub fn is_kill_switch_active(&self) -> bool {
        self.kill_switch_active
    }

    fn should_trigger_kill_switch(&self, coherence: f64) -> bool {
        self.calibration_drift > self.calibration_drift_threshold
            || coherence < 0.2
            || self.consecutive_failures > 5
    }

    fn update_kill_switch(&mut self, coherence: f64) {
        if self.kill_switch_active {
            if self.should_trigger_kill_switch(coherence) {
                self.clear_ticks = 0;
            } else {
                self.clear_ticks += 1;
                if self.clear_ticks >= self.kill_switch_recovery_ticks {
                    self.kill_switch_active = false;
                    self.clear_ticks = 0;
                }
            }
        } else if self.should_trigger_kill_switch(coherence) {
            self.kill_switch_active = true;
            self.clear_ticks = 0;
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
        self.update_kill_switch(state.coherence);

        if self.kill_switch_active {
            return proposals
                .iter()
                .map(|p| Verdict {
                    voter: AgentKind::Metacognitive,
                    proposal_id: p.id,
                    kind: VerdictKind::Reject,
                    confidence: 1.0,
                    objection: Some(format!(
                        "kill-switch active: drift={:.3}, failures={}, coherence={:.2}",
                        self.calibration_drift, self.consecutive_failures, state.coherence
                    )),
                })
                .collect();
        }

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
        if outcomes.is_empty() {
            return;
        }

        let predicted_success = 0.5;
        let actual_success =
            outcomes.iter().filter(|o| o.success).count() as f64 / outcomes.len() as f64;
        let drift_sample = (predicted_success - actual_success).abs();
        self.calibration_drift = self.calibration_drift * 0.8 + drift_sample * 0.2;

        let all_failed = outcomes.iter().all(|o| !o.success);
        if all_failed {
            self.consecutive_failures += 1;
        } else {
            self.consecutive_failures = 0;
        }

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

    #[test]
    fn kill_switch_triggers_on_high_drift() {
        let mut agent =
            MetacognitiveAgent::new(MetacognitivePolicy::default()).with_thresholds(0.3, 3);

        agent.calibration_drift = 0.35;

        let state = make_state(0.8);
        let proposal = Proposal {
            id: 1,
            source: AgentKind::Strategy,
            action: "test".to_string(),
            reasoning: "test".to_string(),
            confidence: 0.8,
            priority: Priority::Normal,
            contradicts: Vec::new(),
            timestamp: Utc::now(),
        };

        let verdicts = agent.deliberate(&[proposal], &state);
        assert_eq!(verdicts[0].kind, VerdictKind::Reject);
        assert!(verdicts[0]
            .objection
            .as_ref()
            .unwrap()
            .contains("kill-switch"));
        assert!(agent.is_kill_switch_active());
    }

    #[test]
    fn kill_switch_triggers_on_low_coherence() {
        let mut agent =
            MetacognitiveAgent::new(MetacognitivePolicy::default()).with_thresholds(0.3, 3);

        let state = make_state(0.1);
        let proposal = Proposal {
            id: 1,
            source: AgentKind::Strategy,
            action: "test".to_string(),
            reasoning: "test".to_string(),
            confidence: 0.8,
            priority: Priority::Normal,
            contradicts: Vec::new(),
            timestamp: Utc::now(),
        };

        let verdicts = agent.deliberate(&[proposal], &state);
        assert_eq!(verdicts[0].kind, VerdictKind::Reject);
        assert!(agent.is_kill_switch_active());
    }

    #[test]
    fn kill_switch_triggers_on_consecutive_failures() {
        let mut agent =
            MetacognitiveAgent::new(MetacognitivePolicy::default()).with_thresholds(0.3, 3);

        agent.consecutive_failures = 6;

        let state = make_state(0.8);
        let proposal = Proposal {
            id: 1,
            source: AgentKind::Strategy,
            action: "test".to_string(),
            reasoning: "test".to_string(),
            confidence: 0.8,
            priority: Priority::Normal,
            contradicts: Vec::new(),
            timestamp: Utc::now(),
        };

        let verdicts = agent.deliberate(&[proposal], &state);
        assert!(agent.is_kill_switch_active());
        assert_eq!(verdicts[0].kind, VerdictKind::Reject);
    }

    #[test]
    fn kill_switch_recovers_after_clear_ticks() {
        let mut agent =
            MetacognitiveAgent::new(MetacognitivePolicy::default()).with_thresholds(0.3, 3);

        agent.kill_switch_active = true;
        agent.calibration_drift = 0.1;
        agent.consecutive_failures = 0;

        let state = make_state(0.8);
        let proposal = Proposal {
            id: 1,
            source: AgentKind::Strategy,
            action: "test".to_string(),
            reasoning: "test".to_string(),
            confidence: 0.8,
            priority: Priority::Normal,
            contradicts: Vec::new(),
            timestamp: Utc::now(),
        };

        for _ in 0..3 {
            agent.deliberate(std::slice::from_ref(&proposal), &state);
        }

        assert!(!agent.is_kill_switch_active());
    }

    #[test]
    fn calibration_drift_updates_on_reflect() {
        let mut agent = MetacognitiveAgent::new(MetacognitivePolicy::default());

        let outcomes = vec![ActionOutcome {
            agent: AgentKind::Strategy,
            proposal_id: 1,
            action: "test".to_string(),
            success: false,
            impact: 0.0,
            learnings: Vec::new(),
            timestamp: Utc::now(),
        }];

        let state = make_state(0.8);
        agent.reflect(&outcomes, &state);

        assert!(agent.calibration_drift > 0.0);
        assert_eq!(agent.consecutive_failures, 1);
    }
}
