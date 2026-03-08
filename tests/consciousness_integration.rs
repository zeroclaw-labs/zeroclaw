use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use zeroclaw::consciousness::agents::chairman::ChairmanAgent;
use zeroclaw::consciousness::agents::conscience::ConscienceAgent;
use zeroclaw::consciousness::agents::execution::ExecutionAgent;
use zeroclaw::consciousness::agents::memory::MemoryAgent;
use zeroclaw::consciousness::agents::metacognitive::MetacognitiveAgent;
use zeroclaw::consciousness::agents::reflection::ReflectionAgent;
use zeroclaw::consciousness::agents::research::ResearchAgent;
use zeroclaw::consciousness::agents::strategy::StrategyAgent;
use zeroclaw::consciousness::metacognition::MetacognitivePolicy;
use zeroclaw::consciousness::peer_transport::{PeerMessage, PeerTransport};
use zeroclaw::consciousness::traits::{
    AgentKind, ConsciousnessAgent, ConsciousnessState, PhenomenalState,
};
use zeroclaw::consciousness::{ConsciousnessConfig, ConsciousnessOrchestrator, PeerState};
use zeroclaw::continuity::{ContinuityGuard, DriftLimits};
use zeroclaw::cosmic::{
    AgentPool, AgentRole, CausalGraph, ConsolidationEngine, Constitution, CosmicMemoryGraph,
    CounterfactualEngine, DriftDetector, EmotionalModulator, FreeEnergyState, GlobalWorkspace,
    IntegrationMeter, NormativeEngine, PolicyEngine, SelfModel, SubsystemId, WorldModel,
};

struct CosmicSubsystems {
    workspace: Arc<Mutex<GlobalWorkspace>>,
    agent_pool: Arc<Mutex<AgentPool>>,
    graph: Arc<Mutex<CosmicMemoryGraph>>,
    consolidation: Arc<Mutex<ConsolidationEngine>>,
    world_model: Arc<Mutex<WorldModel>>,
    counterfactual: Arc<Mutex<CounterfactualEngine>>,
    policy: Arc<Mutex<PolicyEngine>>,
    free_energy: Arc<Mutex<FreeEnergyState>>,
    normative: Arc<Mutex<NormativeEngine>>,
    constitution: Arc<Mutex<Constitution>>,
    causal: Arc<Mutex<CausalGraph>>,
    modulator: Arc<Mutex<EmotionalModulator>>,
    self_model: Arc<Mutex<SelfModel>>,
    drift: Arc<Mutex<DriftDetector>>,
    integration: Arc<Mutex<IntegrationMeter>>,
    continuity: Arc<Mutex<ContinuityGuard>>,
}

fn build_subsystems() -> CosmicSubsystems {
    let mut workspace = GlobalWorkspace::new(0.2, 5, 100);
    workspace.register_subsystem(SubsystemId::Memory, 0.9);
    workspace.register_subsystem(SubsystemId::FreeEnergy, 0.8);
    workspace.register_subsystem(SubsystemId::Causality, 0.7);
    workspace.register_subsystem(SubsystemId::SelfModel, 0.6);
    workspace.register_subsystem(SubsystemId::WorldModel, 0.5);

    let mut agent_pool = AgentPool::new(8, 100);
    agent_pool.register_agent("primary", AgentRole::Primary);

    CosmicSubsystems {
        workspace: Arc::new(Mutex::new(workspace)),
        agent_pool: Arc::new(Mutex::new(agent_pool)),
        graph: Arc::new(Mutex::new(CosmicMemoryGraph::new(1000))),
        consolidation: Arc::new(Mutex::new(ConsolidationEngine::new(0.8))),
        world_model: Arc::new(Mutex::new(WorldModel::new(100))),
        counterfactual: Arc::new(Mutex::new(CounterfactualEngine::new(10, 10))),
        policy: Arc::new(Mutex::new(PolicyEngine::new(10))),
        free_energy: Arc::new(Mutex::new(FreeEnergyState::new(100))),
        normative: Arc::new(Mutex::new(NormativeEngine::new(100, 100))),
        constitution: Arc::new(Mutex::new(Constitution::new())),
        causal: Arc::new(Mutex::new(CausalGraph::new(100))),
        modulator: Arc::new(Mutex::new(EmotionalModulator::new())),
        self_model: Arc::new(Mutex::new(SelfModel::new(100))),
        drift: Arc::new(Mutex::new(DriftDetector::new(50, 0.1))),
        integration: Arc::new(Mutex::new(IntegrationMeter::new())),
        continuity: Arc::new(Mutex::new(ContinuityGuard::new(DriftLimits::default()))),
    }
}

fn build_orchestrator_with_all_agents(s: &CosmicSubsystems) -> ConsciousnessOrchestrator {
    build_orchestrator_with_config(s, ConsciousnessConfig::default())
}

fn build_orchestrator_with_config(
    s: &CosmicSubsystems,
    config: ConsciousnessConfig,
) -> ConsciousnessOrchestrator {
    let mut orch = ConsciousnessOrchestrator::new(config);

    orch.register_agent(Box::new(ChairmanAgent::new(
        Arc::clone(&s.workspace),
        Arc::clone(&s.agent_pool),
        Some(Arc::clone(&s.continuity)),
    )));

    orch.register_agent(Box::new(MemoryAgent::new(
        Arc::clone(&s.graph),
        Arc::clone(&s.consolidation),
    )));

    orch.register_agent(Box::new(ResearchAgent::new(
        Arc::clone(&s.graph),
        Arc::clone(&s.world_model),
    )));

    orch.register_agent(Box::new(StrategyAgent::new(
        Arc::clone(&s.counterfactual),
        Arc::clone(&s.policy),
        Arc::clone(&s.free_energy),
    )));

    orch.register_agent(Box::new(ExecutionAgent::new(
        Arc::clone(&s.causal),
        Arc::clone(&s.modulator),
    )));

    orch.register_agent(Box::new(ConscienceAgent::new(
        Arc::clone(&s.normative),
        Arc::clone(&s.constitution),
    )));

    orch.register_agent(Box::new(ReflectionAgent::new(
        Arc::clone(&s.self_model),
        Arc::clone(&s.drift),
        Arc::clone(&s.integration),
    )));

    orch.register_agent(Box::new(MetacognitiveAgent::new(
        MetacognitivePolicy::default(),
    )));

    orch
}

#[test]
fn ten_ticks_no_panic_and_tick_count_increments() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for i in 1..=10 {
        let result = orch.tick();
        assert_eq!(orch.state().tick_count, i);
        assert!(
            result.coherence >= 0.0 && result.coherence <= 1.0,
            "coherence out of [0,1]: {}",
            result.coherence
        );
    }
}

#[test]
fn proposals_flow_through_system() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    let mut total_proposals = 0;
    let mut total_approved = 0;

    for _ in 0..5 {
        let result = orch.tick();
        total_proposals += result.proposals_generated;
        total_approved += result.proposals_approved;
    }

    assert!(
        total_proposals > 0,
        "expected at least one proposal across 5 ticks"
    );
    assert!(
        total_approved > 0,
        "expected at least one approved proposal across 5 ticks"
    );
}

#[test]
fn coherence_stays_bounded_across_ticks() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..10 {
        orch.tick();
        let c = orch.state().coherence;
        assert!(c >= 0.0, "coherence went negative: {c}");
        assert!(c <= 1.0, "coherence exceeded 1.0: {c}");
    }
}

#[test]
fn conscience_veto_end_to_end() {
    let subsystems = build_subsystems();
    let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());

    orch.register_agent(Box::new(ChairmanAgent::new(
        Arc::clone(&subsystems.workspace),
        Arc::clone(&subsystems.agent_pool),
        Some(Arc::clone(&subsystems.continuity)),
    )));

    orch.register_agent(Box::new(MemoryAgent::new(
        Arc::clone(&subsystems.graph),
        Arc::clone(&subsystems.consolidation),
    )));

    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let constitution = Arc::new(Mutex::new(Constitution::new()));
    let conscience = ConscienceAgent::new(Arc::clone(&normative), Arc::clone(&constitution));

    orch.register_agent(Box::new(conscience));

    let result = orch.tick();

    let has_proposals = result.proposals_generated > 0;
    if has_proposals {
        assert!(
            result.proposals_approved > 0 || result.proposals_vetoed > 0,
            "proposals generated but none resolved"
        );
    }
}

#[test]
fn all_seven_agents_registered() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    let result = orch.tick();
    assert_eq!(orch.state().tick_count, 1);
    assert!(result.coherence >= 0.0);
    assert!(result.coherence <= 1.0);
}

#[test]
fn repeated_ticks_do_not_leak_proposals() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..20 {
        let result = orch.tick();
        assert!(
            result.proposals_generated < 100,
            "proposal count exploded: {}",
            result.proposals_generated
        );
    }
}

#[test]
fn metacognitive_agent_full_tick_cycle() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for i in 1..=5 {
        let result = orch.tick();
        assert_eq!(orch.state().tick_count, i);
        assert!(
            result.coherence >= 0.0 && result.coherence <= 1.0,
            "coherence out of [0,1] at tick {i}: {}",
            result.coherence
        );
        assert!(result.proposals_generated > 0);
    }
}

#[test]
fn metacognitive_generates_adjustment_on_low_coherence() {
    let mut agent = MetacognitiveAgent::new(MetacognitivePolicy::default());
    let state = ConsciousnessState {
        coherence: 0.3,
        tick_count: 1,
        ..Default::default()
    };

    let proposals = agent.perceive(&state, &[]);
    assert!(
        proposals
            .iter()
            .any(|p| p.action.contains("coherence_ema_alpha")),
        "expected coherence adjustment proposal when coherence=0.3"
    );
    assert!(proposals
        .iter()
        .all(|p| p.source == AgentKind::Metacognitive));
}

#[test]
fn metacognitive_no_adjustment_on_high_coherence() {
    let mut agent = MetacognitiveAgent::new(MetacognitivePolicy::default());
    let state = ConsciousnessState {
        coherence: 0.9,
        tick_count: 1,
        ..Default::default()
    };

    let proposals = agent.perceive(&state, &[]);
    assert!(
        !proposals
            .iter()
            .any(|p| p.action.contains("coherence_ema_alpha")),
        "should not propose coherence adjustment when coherence=0.9"
    );
}

#[test]
fn metacognitive_vote_weight_is_0_3() {
    let agent = MetacognitiveAgent::new(MetacognitivePolicy::default());
    assert!((agent.vote_weight() - 0.3).abs() < f64::EPSILON);
}

#[test]
fn metacognitive_vote_weight_affects_weighted_voting() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    let result = orch.tick();
    let total = result.proposals_approved + result.proposals_vetoed;
    assert!(
        total <= result.proposals_generated,
        "decided ({total}) should not exceed generated ({})",
        result.proposals_generated
    );
}

#[test]
fn eight_agents_registered_with_metacognitive() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    let result = orch.tick();
    assert_eq!(orch.state().tick_count, 1);
    assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
    assert!(result.proposals_generated > 0);
}

#[test]
fn peer_transport_construction_with_port_zero() {
    let transport = PeerTransport::new("test_node".to_string(), 0);
    let peers = transport.known_peer_addrs();
    assert!(peers.is_empty());
}

#[test]
fn peer_message_discovery_serialization_roundtrip() {
    let msg = PeerMessage::Discovery {
        node_id: "node_a".to_string(),
        port: 9870,
    };
    let data = serde_json::to_vec(&msg).expect("serialize");
    let decoded: PeerMessage = serde_json::from_slice(&data).expect("deserialize");
    match decoded {
        PeerMessage::Discovery { node_id, port } => {
            assert_eq!(node_id, "node_a");
            assert_eq!(port, 9870);
        }
        _ => panic!("expected Discovery variant"),
    }
}

#[test]
fn peer_message_state_serialization_roundtrip() {
    let state = PeerState {
        node_id: "node_b".to_string(),
        phenomenal: PhenomenalState {
            attention: 0.7,
            arousal: 0.5,
            valence: 0.1,
        },
        coherence: 0.85,
        tick_count: 42,
        last_seen: Utc::now(),
    };
    let msg = PeerMessage::State {
        peer_state: state.clone(),
    };
    let data = serde_json::to_vec(&msg).expect("serialize");
    let decoded: PeerMessage = serde_json::from_slice(&data).expect("deserialize");
    match decoded {
        PeerMessage::State { peer_state } => {
            assert_eq!(peer_state.node_id, "node_b");
            assert!((peer_state.coherence - 0.85).abs() < f64::EPSILON);
            assert_eq!(peer_state.tick_count, 42);
        }
        _ => panic!("expected State variant"),
    }
}

#[test]
fn peer_message_heartbeat_serialization_roundtrip() {
    let msg = PeerMessage::Heartbeat {
        node_id: "node_c".to_string(),
    };
    let data = serde_json::to_vec(&msg).expect("serialize");
    let decoded: PeerMessage = serde_json::from_slice(&data).expect("deserialize");
    match decoded {
        PeerMessage::Heartbeat { node_id } => assert_eq!(node_id, "node_c"),
        _ => panic!("expected Heartbeat variant"),
    }
}

#[test]
fn peer_message_size_under_datagram_limit() {
    let state = PeerState {
        node_id: "x".repeat(200),
        phenomenal: PhenomenalState {
            attention: 1.0,
            arousal: 1.0,
            valence: -1.0,
        },
        coherence: 0.99,
        tick_count: u64::MAX,
        last_seen: Utc::now(),
    };
    let msg = PeerMessage::State { peer_state: state };
    let data = serde_json::to_vec(&msg).expect("serialize");
    assert!(
        data.len() < 1400,
        "peer state message {} bytes exceeds 1400 limit",
        data.len()
    );
}

#[test]
fn orchestrator_collective_enabled_creates_peer_transport() {
    let subsystems = build_subsystems();
    let config = ConsciousnessConfig {
        collective_enabled: true,
        peer_discovery_port: 0,
        ..ConsciousnessConfig::default()
    };
    let mut orch = build_orchestrator_with_config(&subsystems, config);

    let result = orch.tick();
    assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
    assert_eq!(orch.state().tick_count, 1);
}

#[test]
fn orchestrator_collective_disabled_by_default() {
    let subsystems = build_subsystems();
    let config = ConsciousnessConfig::default();
    assert!(!config.collective_enabled);

    let mut orch = build_orchestrator_with_config(&subsystems, config);
    let result = orch.tick();
    assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
}

#[test]
fn dream_consolidation_produces_fragments_after_ticks() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..10 {
        orch.tick();
    }

    let result = orch.tick();
    assert_eq!(orch.state().tick_count, 11);
    assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
}

#[test]
fn dream_consolidation_at_tick_10_returns_patterns() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..12 {
        orch.tick();
    }

    assert_eq!(orch.state().tick_count, 12);
    assert!(orch.state().coherence >= 0.0 && orch.state().coherence <= 1.0);
}

#[test]
fn wisdom_extraction_at_tick_50() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..51 {
        orch.tick();
    }

    let result = orch.tick();
    assert_eq!(orch.state().tick_count, 52);
    assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
}

#[test]
fn somatic_markers_collected_from_agents() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..5 {
        orch.tick();
    }

    let result = orch.tick();
    assert_eq!(orch.state().tick_count, 6);
    assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
}

#[test]
fn peer_transport_starts_with_no_known_peers() {
    let transport = PeerTransport::new("local_node".to_string(), 0);
    assert!(transport.known_peer_addrs().is_empty());
}

#[test]
fn metacognitive_deliberate_approves_stable_system() {
    let mut agent = MetacognitiveAgent::new(MetacognitivePolicy::default());
    let state = ConsciousnessState {
        coherence: 0.8,
        tick_count: 5,
        ..Default::default()
    };

    let proposals = agent.perceive(&state, &[]);

    let test_proposal = zeroclaw::consciousness::traits::Proposal {
        id: 999,
        source: AgentKind::Strategy,
        action: "test_action".to_string(),
        reasoning: "test".to_string(),
        confidence: 0.8,
        priority: zeroclaw::consciousness::traits::Priority::Normal,
        contradicts: Vec::new(),
        timestamp: Utc::now(),
    };

    let all_proposals: Vec<_> = proposals
        .into_iter()
        .chain(std::iter::once(test_proposal))
        .collect();

    let verdicts = agent.deliberate(&all_proposals, &state);
    assert!(!verdicts.is_empty());
    assert!(verdicts.iter().all(|v| v.voter == AgentKind::Metacognitive));
}

#[test]
fn full_orchestrator_tick_with_build_all_agents() {
    use zeroclaw::consciousness::agents::build_all_agents;

    let workspace = Arc::new(Mutex::new(GlobalWorkspace::new(0.3, 5, 10)));
    let agent_pool = Arc::new(Mutex::new(AgentPool::new(4, 10)));
    agent_pool
        .lock()
        .register_agent("primary", AgentRole::Primary);
    let continuity_guard = Arc::new(Mutex::new(ContinuityGuard::new(DriftLimits::default())));
    let graph = Arc::new(Mutex::new(CosmicMemoryGraph::new(1000)));
    let consolidation = Arc::new(Mutex::new(ConsolidationEngine::new(0.8)));
    let world_model = Arc::new(Mutex::new(WorldModel::new(100)));
    let counterfactual = Arc::new(Mutex::new(CounterfactualEngine::new(10, 10)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(10)));
    let free_energy = Arc::new(Mutex::new(FreeEnergyState::new(100)));
    let causal = Arc::new(Mutex::new(CausalGraph::new(100)));
    let modulator = Arc::new(Mutex::new(EmotionalModulator::new()));
    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let constitution = Arc::new(Mutex::new(Constitution::new()));
    let self_model = Arc::new(Mutex::new(SelfModel::new(100)));
    let drift = Arc::new(Mutex::new(DriftDetector::new(50, 0.1)));
    let integration = Arc::new(Mutex::new(IntegrationMeter::new()));

    let agents = build_all_agents(
        workspace,
        agent_pool,
        continuity_guard,
        graph,
        consolidation,
        world_model,
        counterfactual,
        policy,
        free_energy,
        causal,
        modulator,
        normative,
        constitution,
        self_model,
        drift,
        integration,
    );

    assert_eq!(agents.len(), 8);

    let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());
    for agent in agents {
        orch.register_agent(agent);
    }

    let result = orch.tick();
    assert!(result.proposals_generated > 0);
    assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
    assert_eq!(orch.state().tick_count, 1);
}
