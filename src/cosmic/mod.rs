pub mod causality;
pub mod consolidation;
pub mod constitution;
#[cfg(feature = "x0-legacy")]
pub mod counterfactual;
pub mod drift;
pub mod free_energy;
// gate + persistence depend on the gated counterfactual + workspace
// types; cascade-gated.
#[cfg(feature = "x0-legacy")]
pub mod gate;
pub mod integration;
pub mod memory;
pub mod modulation;
pub mod multi_agent;
pub mod normative;
#[cfg(feature = "x0-legacy")]
pub mod persistence;
pub mod policy;
pub mod self_model;
pub mod thalamus;
#[cfg(feature = "x0-legacy")]
pub mod workspace;

pub use causality::{CausalEdge, CausalEvent, CausalGraph, CausalLoop};
pub use consolidation::{ConsolidationEngine, ConsolidationResult, MemoryEntry, MemoryPattern};
pub use constitution::{Constitution, IntegrityCheck, Value};
#[cfg(feature = "x0-legacy")]
pub use counterfactual::{CounterfactualEngine, Scenario, SimulationResult};
pub use drift::{DriftAlert, DriftDetector, DriftReport, DriftSample};
pub use free_energy::{FreeEnergyState, Observation, Prediction, PredictionError};
#[cfg(feature = "x0-legacy")]
pub use gate::{CosmicGate, GateDecision};
pub use integration::{IntegrationMeter, IntegrationSnapshot, SubsystemState};
pub use memory::{CosmicEdge, CosmicMemoryGraph, CosmicNode, spreading_activation};
pub use modulation::{BehavioralBias, EmotionalModulator, GlobalVariable, ModulationSnapshot};
pub use multi_agent::{AgentEntry, AgentPool, AgentRole, ConsensusResult};
pub use normative::{Norm, NormKind, NormViolation, NormativeEngine};
#[cfg(feature = "x0-legacy")]
pub use persistence::{CosmicPersistence, CosmicSnapshot, PersistenceError, gather_snapshot};
pub use policy::{Policy, PolicyDecision, PolicyEngine, PolicyLayer};
pub use self_model::{BeliefSource, SelfBelief, SelfModel, WorldBelief, WorldModel};
pub use thalamus::{InputSignal, SalienceScore, SensoryThalamus, SignalSource, ThalamusSnapshot};
#[cfg(feature = "x0-legacy")]
pub use workspace::{
    BroadcastResult, ConflictResolution, GlobalWorkspace, SubsystemId, WorkspaceEntry,
};
