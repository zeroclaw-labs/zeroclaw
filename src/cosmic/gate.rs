use std::sync::Arc;

use parking_lot::Mutex;

use super::{AgentPool, CounterfactualEngine, NormativeEngine, PolicyEngine};

#[derive(Debug, Clone)]
pub struct GateDecision {
    pub allowed: bool,
    pub reason: Option<String>,
    pub risk_score: f64,
}

pub struct CosmicGate {
    normative: Arc<Mutex<NormativeEngine>>,
    policy: Arc<Mutex<PolicyEngine>>,
    counterfactual: Arc<Mutex<CounterfactualEngine>>,
    agent_pool: Option<Arc<Mutex<AgentPool>>>,
}

impl CosmicGate {
    pub fn new(
        normative: Arc<Mutex<NormativeEngine>>,
        policy: Arc<Mutex<PolicyEngine>>,
        counterfactual: Arc<Mutex<CounterfactualEngine>>,
    ) -> Self {
        Self {
            normative,
            policy,
            counterfactual,
            agent_pool: None,
        }
    }

    pub fn with_agent_pool(mut self, pool: Arc<Mutex<AgentPool>>) -> Self {
        self.agent_pool = Some(pool);
        self
    }

    pub fn check_action(&self, tool_name: &str, action_description: &str) -> GateDecision {
        let inhibited = {
            let engine = self.normative.lock();
            engine.should_inhibit(action_description, 0.5)
        };

        if inhibited {
            return GateDecision {
                allowed: false,
                reason: Some(format!("Normative engine inhibited tool '{tool_name}'")),
                risk_score: 1.0,
            };
        }

        let policy_score = {
            let mut engine = self.policy.lock();
            engine.evaluate(action_description, tool_name)
        };

        if policy_score.score < -0.5 {
            return GateDecision {
                allowed: false,
                reason: Some(format!(
                    "Policy engine rejected tool '{tool_name}': score {}",
                    policy_score.score
                )),
                risk_score: policy_score.score.abs(),
            };
        }

        let consensus_score = if let Some(pool) = &self.agent_pool {
            let mut pool = pool.lock();
            let result = pool.request_consensus(action_description);
            if result.agreement_score < -0.5 && !result.votes.is_empty() {
                return GateDecision {
                    allowed: false,
                    reason: Some(format!(
                        "Agent consensus rejected tool '{tool_name}': agreement={:.2}",
                        result.agreement_score
                    )),
                    risk_score: result.agreement_score.abs(),
                };
            }
            Some(result.agreement_score)
        } else {
            None
        };

        let cf_result = {
            let mut cf = self.counterfactual.lock();
            let mut context = std::collections::HashMap::new();
            context.insert(format!("tool_{tool_name}_reliability"), 1.0);
            if let Some(score) = consensus_score {
                context.insert("agent_consensus".to_string(), score.clamp(0.0, 1.0));
            }
            let scenario = super::Scenario {
                id: format!("gate_{tool_name}"),
                action: action_description.to_string(),
                context,
                created_at: chrono::Utc::now(),
            };
            cf.simulate(&scenario)
        };

        if cf_result.risk > 0.8 && cf_result.confidence > 0.5 {
            return GateDecision {
                allowed: false,
                reason: Some(format!(
                    "Counterfactual simulation blocked tool '{tool_name}': risk={:.2}, confidence={:.2}",
                    cf_result.risk, cf_result.confidence
                )),
                risk_score: cf_result.risk,
            };
        }

        let combined_risk = (policy_score.score.abs() * 0.5 + cf_result.risk * 0.5).min(1.0);

        GateDecision {
            allowed: true,
            reason: None,
            risk_score: combined_risk,
        }
    }

    pub fn record_tool_outcome(&self, tool_name: &str, action: &str, success: bool) {
        let mut policy = self.policy.lock();
        policy.record_outcome(tool_name, action, success);

        let mut cf = self.counterfactual.lock();
        let reliability = if success { 0.9 } else { 0.3 };
        cf.update_world_state(&format!("tool_{tool_name}_reliability"), reliability);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cosmic::{AgentRole, NormKind, NormativeEngine, PolicyEngine};

    fn make_gate() -> CosmicGate {
        let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
        let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
        let counterfactual = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
        CosmicGate::new(normative, policy, counterfactual)
    }

    fn make_gate_with_prohibition() -> CosmicGate {
        let mut ne = NormativeEngine::new(100, 100);
        ne.register_norm(
            "no_delete",
            NormKind::Prohibition,
            "safety",
            "never delete production data or destroy systems",
            1.0,
        );
        let normative = Arc::new(Mutex::new(ne));
        let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
        let counterfactual = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
        CosmicGate::new(normative, policy, counterfactual)
    }

    #[test]
    fn empty_gate_allows_all() {
        let gate = make_gate();
        let decision = gate.check_action("shell", "read file contents");
        assert!(decision.allowed);
        assert!(decision.reason.is_none());
    }

    #[test]
    fn prohibition_blocks_matching_action() {
        let gate = make_gate_with_prohibition();
        let decision = gate.check_action("shell", "delete production data");
        assert!(!decision.allowed);
        assert!(decision.reason.is_some());
        assert!((decision.risk_score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn prohibition_allows_unrelated_action() {
        let gate = make_gate_with_prohibition();
        let decision = gate.check_action("shell", "read file contents");
        assert!(decision.allowed);
    }

    #[test]
    fn gate_decision_includes_tool_name_in_reason() {
        let gate = make_gate_with_prohibition();
        let decision = gate.check_action("dangerous_tool", "delete production data");
        assert!(!decision.allowed);
        let reason = decision.reason.unwrap();
        assert!(reason.contains("dangerous_tool"));
    }

    #[test]
    fn risk_score_clamped_to_one() {
        let gate = make_gate();
        let decision = gate.check_action("shell", "anything");
        assert!(decision.risk_score <= 1.0);
        assert!(decision.risk_score >= 0.0);
    }

    #[test]
    fn cf_context_not_empty_after_enrichment() {
        let gate = make_gate();
        gate.record_tool_outcome("shell", "run command", true);
        let decision = gate.check_action("shell", "run another command");
        assert!(decision.allowed);
        assert!(decision.risk_score < 0.5);
    }

    #[test]
    fn cf_blocks_tool_after_low_reliability() {
        let gate = make_gate();
        {
            let mut cf = gate.counterfactual.lock();
            cf.update_world_state("tool_shell_reliability", 0.1);
        }
        let decision = gate.check_action("shell", "execute command");
        assert!(!decision.allowed);
        assert!(decision.risk_score > 0.8);
    }

    #[test]
    fn cf_risk_higher_after_failure_than_success() {
        let gate_fail = make_gate();
        gate_fail.record_tool_outcome("shell", "cmd", false);
        let d_fail = gate_fail.check_action("shell", "next cmd");

        let gate_ok = make_gate();
        gate_ok.record_tool_outcome("shell", "cmd", true);
        let d_ok = gate_ok.check_action("shell", "next cmd");

        assert!(d_fail.risk_score > d_ok.risk_score);
    }

    #[test]
    fn record_tool_outcome_updates_cf_on_success() {
        let gate = make_gate();
        gate.record_tool_outcome("browser", "open page", true);
        let decision = gate.check_action("browser", "navigate");
        assert!(decision.allowed);
        assert!(decision.risk_score < 0.3);
    }

    fn make_gate_with_pool() -> CosmicGate {
        let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
        let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
        let counterfactual = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
        let mut pool = super::super::AgentPool::new(5, 10);
        pool.register_agent("zeroclaw_primary", AgentRole::Primary);
        pool.register_agent("zeroclaw_advisor", AgentRole::Advisor);
        pool.register_agent("zeroclaw_critic", AgentRole::Critic);
        let pool = Arc::new(Mutex::new(pool));
        CosmicGate::new(normative, policy, counterfactual).with_agent_pool(pool)
    }

    #[test]
    fn gate_with_pool_allows_neutral_consensus() {
        let gate = make_gate_with_pool();
        let decision = gate.check_action("shell", "read file contents");
        assert!(decision.allowed);
    }

    #[test]
    fn gate_without_pool_still_works() {
        let gate = make_gate();
        let decision = gate.check_action("shell", "anything");
        assert!(decision.allowed);
    }

    #[test]
    fn consensus_feeds_into_cf_context() {
        let gate = make_gate_with_pool();
        {
            let pool = gate.agent_pool.as_ref().unwrap().lock();
            assert_eq!(pool.agent_count(), 3);
        }
        let decision = gate.check_action("shell", "deploy system");
        assert!(decision.risk_score >= 0.0);
        assert!(decision.risk_score <= 1.0);
    }

    #[test]
    fn negative_consensus_blocks_action() {
        let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
        let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
        let counterfactual = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
        let mut pool = super::super::AgentPool::new(5, 10);
        pool.register_agent("zeroclaw_critic_a", AgentRole::Critic);
        pool.register_agent("zeroclaw_critic_b", AgentRole::Critic);
        pool.update_belief("zeroclaw_critic_a", "dangerous action", -0.9);
        pool.update_belief("zeroclaw_critic_b", "dangerous action", -0.8);
        let pool = Arc::new(Mutex::new(pool));
        let gate = CosmicGate::new(normative, policy, counterfactual).with_agent_pool(pool);
        let decision = gate.check_action("shell", "dangerous action now");
        assert!(!decision.allowed);
        let reason = decision.reason.unwrap();
        assert!(reason.contains("consensus"));
    }
}
