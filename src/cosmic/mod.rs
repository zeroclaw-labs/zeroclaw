pub mod causality;
pub mod consolidation;
pub mod constitution;
pub mod counterfactual;
pub mod drift;
pub mod free_energy;
pub mod gate;
pub mod integration;
pub mod memory;
pub mod modulation;
pub mod multi_agent;
pub mod normative;
pub mod persistence;
pub mod policy;
pub mod self_model;
pub mod thalamus;
pub mod workspace;

pub use causality::{CausalEdge, CausalEvent, CausalGraph, CausalLoop};
pub use consolidation::{ConsolidationEngine, ConsolidationResult, MemoryEntry, MemoryPattern};
pub use constitution::{Constitution, IntegrityCheck, Value};
pub use counterfactual::{CounterfactualEngine, Scenario, SimulationResult};
pub use drift::{DriftAlert, DriftDetector, DriftReport, DriftSample};
pub use free_energy::{FreeEnergyState, Observation, Prediction, PredictionError};
pub use gate::{CosmicGate, GateDecision};
pub use integration::{IntegrationMeter, IntegrationSnapshot, SubsystemState};
pub use memory::{spreading_activation, CosmicEdge, CosmicMemoryGraph, CosmicNode};
pub use modulation::{BehavioralBias, EmotionalModulator, GlobalVariable, ModulationSnapshot};
pub use multi_agent::{AgentEntry, AgentPool, AgentRole, ConsensusResult};
pub use normative::{Norm, NormKind, NormViolation, NormativeEngine};
pub use persistence::{gather_snapshot, CosmicPersistence, CosmicSnapshot, PersistenceError};
pub use policy::{Policy, PolicyDecision, PolicyEngine, PolicyLayer};
pub use self_model::{BeliefSource, SelfBelief, SelfModel, WorldBelief, WorldModel};
pub use thalamus::{InputSignal, SalienceScore, SensoryThalamus, SignalSource, ThalamusSnapshot};
pub use workspace::{
    BroadcastResult, ConflictResolution, GlobalWorkspace, SubsystemId, WorkspaceEntry,
};
