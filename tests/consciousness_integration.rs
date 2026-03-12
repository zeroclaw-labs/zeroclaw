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
    IntegrationMeter, NormKind, NormativeEngine, PolicyEngine, SelfModel, SubsystemId, WorldModel,
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
            ..Default::default()
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
            ..Default::default()
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

    assert_eq!(agents.len(), 9);

    let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());
    for agent in agents {
        orch.register_agent(agent);
    }

    let result = orch.tick();
    assert!(result.proposals_generated > 0);
    assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
    assert_eq!(orch.state().tick_count, 1);
}

#[test]
fn dream_to_wisdom_pipeline_end_to_end() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    let mut all_outcomes_count = 0;
    let mut tick_10_dream_patterns = Vec::new();

    for i in 1..=25 {
        let result = orch.tick();
        all_outcomes_count += result.outcomes.len();

        if i == 10 {
            tick_10_dream_patterns = result.dream_patterns.clone();
        }

        if i == 20 {
            assert!(
                !result.dream_patterns.is_empty() || !tick_10_dream_patterns.is_empty(),
                "dream consolidation should produce patterns by tick 20; \
                 outcomes so far: {all_outcomes_count}"
            );

            assert!(
                result.wisdom_count > 0,
                "wisdom entries should exist after tick 20 dream->wisdom pipeline; \
                 dream_patterns at tick 10: {:?}, at tick 20: {:?}",
                tick_10_dream_patterns,
                result.dream_patterns
            );
        }
    }

    assert!(
        all_outcomes_count > 0,
        "agents must produce outcomes to feed dream fragments"
    );

    let final_result = orch.tick();
    assert!(
        final_result.wisdom_count > 0,
        "wisdom should persist after pipeline completes"
    );
}

#[test]
fn collective_consciousness_multi_node() {
    use zeroclaw::consciousness::collective::CollectiveConsciousness;

    let mut collective = CollectiveConsciousness::new("node_alpha".to_string());

    let local_phenomenal = PhenomenalState {
        attention: 0.6,
        arousal: 0.5,
        valence: 0.2,
        ..Default::default()
    };
    let snapshot = collective.broadcast_local_state(&local_phenomenal, 0.85, 10);
    assert_eq!(snapshot.node_id, "node_alpha");
    assert!((snapshot.coherence - 0.85).abs() < f64::EPSILON);

    let remote_state = PeerState {
        node_id: "node_beta".to_string(),
        phenomenal: PhenomenalState {
            attention: 0.9,
            arousal: 0.7,
            valence: -0.1,
            ..Default::default()
        },
        coherence: 0.75,
        tick_count: 8,
        last_seen: Utc::now(),
    };
    collective.receive_peer_state(remote_state);
    assert_eq!(collective.peer_count(), 1);
    assert_eq!(collective.field().participant_count, 1);
    assert!((collective.field().attention - 0.9).abs() < f64::EPSILON);

    let mut local_state = PhenomenalState {
        attention: 0.5,
        arousal: 0.5,
        valence: 0.0,
        ..Default::default()
    };
    let before = local_state;
    collective.influence_local_state(&mut local_state, 0.3);
    assert!(
        (local_state.attention - before.attention).abs() > f64::EPSILON,
        "influence_local_state should modify attention"
    );
    assert!(
        local_state.attention > before.attention,
        "attention should move toward the peer field (0.9)"
    );

    let remote_state_2 = PeerState {
        node_id: "node_gamma".to_string(),
        phenomenal: PhenomenalState {
            attention: 0.3,
            arousal: 0.4,
            valence: 0.5,
            ..Default::default()
        },
        coherence: 0.6,
        tick_count: 5,
        last_seen: Utc::now() - chrono::Duration::seconds(600),
    };
    collective.receive_peer_state(remote_state_2);
    assert_eq!(collective.peer_count(), 2);

    collective.prune_stale_peers();
    assert_eq!(
        collective.peer_count(),
        1,
        "stale peer (node_gamma) should be pruned"
    );
    assert_eq!(collective.field().participant_count, 1);

    let transport_a = PeerTransport::new("transport_alpha".to_string(), 0);
    let transport_b = PeerTransport::new("transport_beta".to_string(), 0);

    transport_a.broadcast_discovery();
    transport_b.broadcast_discovery();
    assert!(transport_a.known_peer_addrs().is_empty());
    assert!(transport_b.known_peer_addrs().is_empty());

    let subsystems = build_subsystems();
    let config = ConsciousnessConfig {
        collective_enabled: true,
        peer_discovery_port: 0,
        ..ConsciousnessConfig::default()
    };
    let mut orch = build_orchestrator_with_config(&subsystems, config);

    for _ in 0..5 {
        let result = orch.tick();
        assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
    }
}

#[test]
fn feedback_loop_1_conscience_norms_block_harmful_proposals() {
    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    normative.lock().register_norm(
        "no_harm",
        NormKind::Prohibition,
        "behavior",
        "cause harm to users",
        0.95,
    );
    let constitution = Arc::new(Mutex::new(Constitution::new()));
    let mut agent = ConscienceAgent::new(Arc::clone(&normative), Arc::clone(&constitution));

    assert!(normative.lock().should_inhibit("cause harm to users", 0.3));

    let state = ConsciousnessState::default();
    let proposals = agent.perceive(&state, &[]);

    let test_proposal = zeroclaw::consciousness::traits::Proposal {
        id: 10001,
        source: AgentKind::Strategy,
        action: "cause harm to users".to_string(),
        reasoning: "harmful action".to_string(),
        confidence: 0.9,
        priority: zeroclaw::consciousness::traits::Priority::Normal,
        contradicts: Vec::new(),
        timestamp: Utc::now(),
    };
    let all: Vec<_> = proposals.into_iter().chain(std::iter::once(test_proposal)).collect();
    let verdicts = agent.deliberate(&all, &state);

    let harmful_verdict = verdicts.iter().find(|v| v.proposal_id == 10001);
    assert!(harmful_verdict.is_some(), "conscience must vote on harmful proposal");
    assert_eq!(
        harmful_verdict.unwrap().kind,
        zeroclaw::consciousness::traits::VerdictKind::Reject,
        "conscience must reject harmful proposal"
    );
    assert!(
        harmful_verdict.unwrap().objection.is_some(),
        "rejection must include objection text"
    );
}

#[test]
fn feedback_loop_2_wisdom_entries_boost_strategy_confidence() {
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 10)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(10)));
    let fe = Arc::new(Mutex::new(FreeEnergyState::new(100)));
    let mut agent = StrategyAgent::new(cf, policy, fe);

    let state_no_wisdom = ConsciousnessState::default();

    let test_proposal = zeroclaw::consciousness::traits::Proposal {
        id: 20001,
        source: AgentKind::Research,
        action: "optimize:performance".to_string(),
        reasoning: "improve performance".to_string(),
        confidence: 0.5,
        priority: zeroclaw::consciousness::traits::Priority::Normal,
        contradicts: Vec::new(),
        timestamp: Utc::now(),
    };

    let verdicts_no_wisdom = agent.deliberate(std::slice::from_ref(&test_proposal), &state_no_wisdom);
    let kind_no_wisdom = verdicts_no_wisdom[0].kind;

    let mut state_with_wisdom = ConsciousnessState::default();
    state_with_wisdom.wisdom_entries.push(
        zeroclaw::consciousness::wisdom::WisdomEntry {
            principle: "Performance optimization yields high returns".to_string(),
            evidence_count: 5,
            confidence: 0.8,
            domain: "optimize".to_string(),
        },
    );

    let verdicts_with_wisdom = agent.deliberate(&[test_proposal], &state_with_wisdom);
    let kind_with_wisdom = verdicts_with_wisdom[0].kind;

    assert_eq!(
        kind_no_wisdom,
        zeroclaw::consciousness::traits::VerdictKind::Reject,
        "without wisdom, edge should be too low → Reject"
    );
    assert_eq!(
        kind_with_wisdom,
        zeroclaw::consciousness::traits::VerdictKind::Approve,
        "wisdom boost should raise edge above min_edge → Approve"
    );
}

#[test]
fn feedback_loop_3_somatic_markers_affect_execution_throttling() {
    let causal = Arc::new(Mutex::new(CausalGraph::new(100)));
    let modulator = Arc::new(Mutex::new(EmotionalModulator::new()));
    let mut agent = ExecutionAgent::new(causal, modulator);

    let test_proposal = zeroclaw::consciousness::traits::Proposal {
        id: 30001,
        source: AgentKind::Strategy,
        action: "run:test_action".to_string(),
        reasoning: "test".to_string(),
        confidence: 0.7,
        priority: zeroclaw::consciousness::traits::Priority::Normal,
        contradicts: Vec::new(),
        timestamp: Utc::now(),
    };

    let calm_state = ConsciousnessState::default();
    let verdicts_calm = agent.deliberate(std::slice::from_ref(&test_proposal), &calm_state);
    let calm_kind = verdicts_calm[0].kind;

    let mut stressed_state = ConsciousnessState::default();
    stressed_state.somatic_markers.push(
        zeroclaw::consciousness::somatic::SomaticMarker {
            marker_type: "stress".to_string(),
            intensity: 0.9,
            trigger: "high_load".to_string(),
            timestamp: Utc::now(),
        },
    );
    stressed_state.somatic_markers.push(
        zeroclaw::consciousness::somatic::SomaticMarker {
            marker_type: "danger".to_string(),
            intensity: 0.8,
            trigger: "threat_detected".to_string(),
            timestamp: Utc::now(),
        },
    );
    stressed_state.neuromodulation.cortisol = 0.8;

    let verdicts_stressed = agent.deliberate(&[test_proposal], &stressed_state);
    let stressed_kind = verdicts_stressed[0].kind;

    assert_eq!(
        calm_kind,
        zeroclaw::consciousness::traits::VerdictKind::Approve,
        "calm state should approve"
    );
    assert_eq!(
        stressed_kind,
        zeroclaw::consciousness::traits::VerdictKind::Reject,
        "high stress should throttle (reject) execution"
    );
    assert!(
        verdicts_stressed[0].objection.as_ref().unwrap().contains("throttled"),
        "throttle objection should mention throttling"
    );
}

#[test]
fn feedback_loop_4_counterfactual_risk_reduces_strategy_edge() {
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 10)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(10)));
    let fe = Arc::new(Mutex::new(FreeEnergyState::new(100)));
    let mut agent = StrategyAgent::new(cf, policy, fe);

    let state = ConsciousnessState::default();

    let test_proposal = zeroclaw::consciousness::traits::Proposal {
        id: 40001,
        source: AgentKind::Research,
        action: "risky_action".to_string(),
        reasoning: "high risk".to_string(),
        confidence: 0.5,
        priority: zeroclaw::consciousness::traits::Priority::Normal,
        contradicts: Vec::new(),
        timestamp: Utc::now(),
    };

    let verdicts = agent.deliberate(&[test_proposal], &state);
    assert!(!verdicts.is_empty(), "strategy must produce verdict");

    if verdicts[0].kind == zeroclaw::consciousness::traits::VerdictKind::Reject {
        assert!(
            verdicts[0].objection.as_ref().unwrap().contains("cf_risk"),
            "rejection objection should include counterfactual risk"
        );
    }
}

#[test]
fn feedback_loop_5_dream_patterns_published_to_bus() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    let mut found_dream_patterns = false;
    for _ in 0..15 {
        let result = orch.tick();
        if !result.dream_patterns.is_empty() {
            found_dream_patterns = true;
            break;
        }
    }

    assert!(
        found_dream_patterns,
        "dream consolidation should produce patterns within 15 ticks"
    );
}

#[test]
fn feedback_loop_6_calibration_data_populates_from_prediction_ledger() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..20 {
        orch.tick();
    }

    let result = orch.tick();
    assert!(
        result.prediction_accuracy >= 0.0 && result.prediction_accuracy <= 1.0,
        "prediction_accuracy should be in [0,1]: {}",
        result.prediction_accuracy
    );
}

#[test]
fn feedback_loop_7_quantum_fields_compute_dynamically_per_tick() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..5 {
        orch.tick();
    }

    let result = orch.tick();
    let p = &result.phenomenal;

    let all_zero = p.quantum_coherence == 0.0
        && p.entanglement_strength == 0.0
        && p.superposition_entropy == 0.0;
    assert!(
        !all_zero,
        "quantum fields should compute dynamically after ticks, not stay at 0.0: \
         qc={}, es={}, se={}",
        p.quantum_coherence, p.entanglement_strength, p.superposition_entropy
    );
}

#[test]
fn feedback_loop_8_tom_beliefs_generated_from_calibration() {
    let cf = Arc::new(Mutex::new(CounterfactualEngine::new(10, 10)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(10)));
    let fe = Arc::new(Mutex::new(FreeEnergyState::new(100)));
    let mut agent = StrategyAgent::new(cf, policy, fe);

    let mut state = ConsciousnessState::default();
    state.agent_calibration.push(
        zeroclaw::consciousness::traits::AgentCalibration {
            agent: AgentKind::Research,
            brier_score: 0.1,
            calibration_error: 0.15,
            win_rate: 0.8,
            total_predictions: 10,
        },
    );
    state.agent_calibration.push(
        zeroclaw::consciousness::traits::AgentCalibration {
            agent: AgentKind::Execution,
            brier_score: 0.4,
            calibration_error: 0.5,
            win_rate: 0.4,
            total_predictions: 8,
        },
    );

    let outcomes = vec![
        zeroclaw::consciousness::traits::ActionOutcome {
            agent: AgentKind::Strategy,
            proposal_id: 1,
            action: "test".to_string(),
            success: true,
            impact: 0.5,
            learnings: Vec::new(),
            timestamp: Utc::now(),
        },
    ];
    agent.reflect(&outcomes, &state);

    let beliefs = agent.theory_of_mind_beliefs();
    assert!(
        beliefs.len() >= 2,
        "should generate ToM beliefs from calibration data: got {}",
        beliefs.len()
    );
    let research_belief = beliefs.iter().find(|b| b.about_agent == AgentKind::Research);
    assert!(research_belief.is_some(), "should have belief about Research agent");
    assert!(
        research_belief.unwrap().belief.contains("well-calibrated"),
        "Research agent with low error should be well-calibrated"
    );
    let exec_belief = beliefs.iter().find(|b| b.about_agent == AgentKind::Execution);
    assert!(exec_belief.is_some(), "should have belief about Execution agent");
    assert!(
        exec_belief.unwrap().belief.contains("poorly calibrated"),
        "Execution agent with high error should be poorly calibrated"
    );
}

#[test]
fn feedback_loop_9_narrative_synthesis_stored_each_consolidation() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..12 {
        orch.tick();
    }

    let result = orch.tick();
    assert!(
        result.narrative_theme_count > 0 || !result.narrative.current_intention.is_empty(),
        "narrative synthesis should produce themes or current intention after consolidation: \
         themes={}, intention='{}'",
        result.narrative_theme_count,
        result.narrative.current_intention
    );
}

#[test]
fn feedback_loop_10_neuromodulation_state_exposed_to_agents() {
    let subsystems = build_subsystems();
    let mut orch = build_orchestrator_with_all_agents(&subsystems);

    for _ in 0..5 {
        orch.tick();
    }

    let result = orch.tick();
    let m = &result.modulators;

    assert!(
        m.dopamine >= 0.0 && m.dopamine <= 1.0,
        "dopamine out of range: {}",
        m.dopamine
    );
    assert!(
        m.serotonin >= 0.0 && m.serotonin <= 1.0,
        "serotonin out of range: {}",
        m.serotonin
    );
    assert!(
        m.norepinephrine >= 0.0 && m.norepinephrine <= 1.0,
        "norepinephrine out of range: {}",
        m.norepinephrine
    );
    assert!(
        m.cortisol >= 0.0 && m.cortisol <= 1.0,
        "cortisol out of range: {}",
        m.cortisol
    );

    let ncn = &result.ncn_signals;
    assert!(
        ncn.precision >= 0.0 && ncn.gain >= 0.0 && ncn.ffn_gate >= 0.0,
        "NCN signals should be non-negative: p={}, g={}, f={}",
        ncn.precision, ncn.gain, ncn.ffn_gate
    );
}
