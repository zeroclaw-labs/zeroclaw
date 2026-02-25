use std::sync::Arc;

use parking_lot::Mutex;

use super::{CounterfactualEngine, NormativeEngine, PolicyEngine};

#[derive(Debug, Clone)]
pub struct GateDecision {
    pub allowed: bool,
    pub reason: Option<String>,
    pub risk_score: f64,
}

pub struct CosmicGate {
    normative: Arc<Mutex<NormativeEngine>>,
    policy: Arc<Mutex<PolicyEngine>>,
    _counterfactual: Arc<Mutex<CounterfactualEngine>>,
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
            _counterfactual: counterfactual,
        }
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

        GateDecision {
            allowed: true,
            reason: None,
            risk_score: policy_score.score.abs().min(1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cosmic::{NormKind, NormativeEngine, PolicyEngine};

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
}
