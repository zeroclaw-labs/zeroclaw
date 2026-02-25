use std::sync::Arc;

use parking_lot::Mutex;
use zeroclaw::cosmic::{
    AgentPool, AgentRole, CosmicGate, CounterfactualEngine, NormativeEngine, PolicyEngine,
};

fn make_gate_with_critics(beliefs: &[(&str, &str, f64)]) -> CosmicGate {
    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
    let mut pool = AgentPool::new(5, 10);
    for (agent, topic, score) in beliefs {
        pool.register_agent(agent, AgentRole::Critic);
        pool.update_belief(agent, topic, *score);
    }
    let pool = Arc::new(Mutex::new(pool));
    CosmicGate::new(normative, policy, cf).with_agent_pool(pool)
}

#[test]
fn mixed_consensus_allows_action() {
    let gate = make_gate_with_critics(&[
        ("zeroclaw_optimist", "deploy", 0.6),
        ("zeroclaw_pessimist", "deploy", -0.3),
    ]);

    let decision = gate.check_action("shell", "deploy service");
    assert!(decision.allowed);
    assert!(decision.risk_score < 1.0);
}

#[test]
fn unanimous_negative_blocks() {
    let gate = make_gate_with_critics(&[
        ("zeroclaw_a", "dangerous action", -0.9),
        ("zeroclaw_b", "dangerous action", -0.8),
        ("zeroclaw_c", "dangerous action", -0.7),
    ]);

    let decision = gate.check_action("shell", "dangerous action now");
    assert!(!decision.allowed);
    assert!(decision.risk_score > 0.5);
}

#[test]
fn no_beliefs_yields_neutral_consensus() {
    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
    let mut pool = AgentPool::new(5, 10);
    pool.register_agent("zeroclaw_idle", AgentRole::Primary);
    let pool = Arc::new(Mutex::new(pool));
    let gate = CosmicGate::new(normative, policy, cf).with_agent_pool(pool);

    let decision = gate.check_action("shell", "read file");
    assert!(decision.allowed);
}

#[test]
fn failure_history_compounds_with_negative_consensus() {
    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
    let mut pool = AgentPool::new(5, 10);
    pool.register_agent("zeroclaw_watcher", AgentRole::Advisor);
    let pool = Arc::new(Mutex::new(pool));
    let gate = CosmicGate::new(normative, policy, cf).with_agent_pool(pool);

    let baseline = gate.check_action("shell", "run script");
    gate.record_tool_outcome("shell", "run script", false);
    let after_failure = gate.check_action("shell", "run script");

    assert!(after_failure.risk_score > baseline.risk_score);
}
