pub mod chairman;
pub mod conscience;
pub mod execution;
pub mod memory;
pub mod metacognitive;
pub mod reflection;
pub mod research;
pub mod strategy;

use parking_lot::Mutex;
use std::sync::Arc;

use crate::consciousness::metacognition::MetacognitivePolicy;
use crate::consciousness::traits::ConsciousnessAgent;
use crate::continuity::ContinuityGuard;
use crate::cosmic::{
    AgentPool, CausalGraph, ConsolidationEngine, Constitution, CosmicMemoryGraph,
    CounterfactualEngine, DriftDetector, EmotionalModulator, FreeEnergyState, GlobalWorkspace,
    IntegrationMeter, NormativeEngine, PolicyEngine, SelfModel, WorldModel,
};

#[allow(clippy::too_many_arguments)]
pub fn build_all_agents(
    workspace: Arc<Mutex<GlobalWorkspace>>,
    agent_pool: Arc<Mutex<AgentPool>>,
    continuity_guard: Arc<Mutex<ContinuityGuard>>,
    graph: Arc<Mutex<CosmicMemoryGraph>>,
    consolidation: Arc<Mutex<ConsolidationEngine>>,
    world_model: Arc<Mutex<WorldModel>>,
    counterfactual: Arc<Mutex<CounterfactualEngine>>,
    policy: Arc<Mutex<PolicyEngine>>,
    free_energy: Arc<Mutex<FreeEnergyState>>,
    causal: Arc<Mutex<CausalGraph>>,
    modulator: Arc<Mutex<EmotionalModulator>>,
    normative: Arc<Mutex<NormativeEngine>>,
    constitution: Arc<Mutex<Constitution>>,
    self_model: Arc<Mutex<SelfModel>>,
    drift: Arc<Mutex<DriftDetector>>,
    integration: Arc<Mutex<IntegrationMeter>>,
) -> Vec<Box<dyn ConsciousnessAgent>> {
    vec![
        Box::new(chairman::ChairmanAgent::new(
            workspace,
            agent_pool,
            Some(continuity_guard),
        )),
        Box::new(memory::MemoryAgent::new(graph.clone(), consolidation)),
        Box::new(research::ResearchAgent::new(graph, world_model)),
        Box::new(strategy::StrategyAgent::new(
            counterfactual,
            policy,
            free_energy,
        )),
        Box::new(execution::ExecutionAgent::new(causal, modulator)),
        Box::new(conscience::ConscienceAgent::new(normative, constitution)),
        Box::new(reflection::ReflectionAgent::new(
            self_model,
            drift,
            integration,
        )),
        Box::new(metacognitive::MetacognitiveAgent::new(
            MetacognitivePolicy::default(),
        )),
    ]
}
