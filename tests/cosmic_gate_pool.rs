use std::sync::Arc;

use parking_lot::Mutex;
use zeroclaw::cosmic::{
    AgentPool, AgentRole, CosmicGate, CounterfactualEngine, NormativeEngine, PolicyEngine,
};

#[test]
fn gate_with_pool_allows_benign_actions() {
    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
    let mut pool = AgentPool::new(5, 10);
    pool.register_agent("zeroclaw_primary", AgentRole::Primary);
    pool.register_agent("zeroclaw_advisor", AgentRole::Advisor);
    let pool = Arc::new(Mutex::new(pool));
    let gate = CosmicGate::new(normative, policy, cf).with_agent_pool(pool);

    let decision = gate.check_action("shell", "read file contents");
    assert!(decision.allowed);
    assert!(decision.risk_score <= 1.0);
}

#[test]
fn gate_pool_negative_consensus_blocks() {
    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
    let mut pool = AgentPool::new(5, 10);
    pool.register_agent("zeroclaw_critic_a", AgentRole::Critic);
    pool.register_agent("zeroclaw_critic_b", AgentRole::Critic);
    pool.update_belief("zeroclaw_critic_a", "dangerous action", -0.9);
    pool.update_belief("zeroclaw_critic_b", "dangerous action", -0.8);
    let pool = Arc::new(Mutex::new(pool));
    let gate = CosmicGate::new(normative, policy, cf).with_agent_pool(pool);

    let decision = gate.check_action("shell", "dangerous action now");
    assert!(!decision.allowed);
    assert!(decision.reason.unwrap().contains("consensus"));
}

#[test]
fn gate_tool_outcome_affects_subsequent_checks() {
    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
    let gate = CosmicGate::new(normative, policy, cf);

    let baseline = gate.check_action("shell", "run command");
    gate.record_tool_outcome("shell", "run command", false);
    let after_failure = gate.check_action("shell", "run command");

    assert!(after_failure.risk_score > baseline.risk_score);
}
