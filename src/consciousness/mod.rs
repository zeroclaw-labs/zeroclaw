pub mod agents;
pub mod bus;
pub mod collective;
pub mod dream;
pub mod metacognition;
pub mod narrative;
pub mod neuromodulation;
pub mod orchestrator;
pub mod peer_transport;
pub mod prediction_market;
pub mod somatic;
pub mod traits;
pub mod wisdom;

pub use bus::{BusMessage, SharedBus};
pub use collective::{CollectiveConsciousness, CollectiveField, PeerState, ResonanceEvent};
pub use dream::{DreamConsolidator, DreamFragment};
pub use metacognition::{
    Adjustment, MetacognitiveEngine, MetacognitiveObservation, MetacognitivePolicy,
};
pub use narrative::{NarrativeEngine, NarrativeTheme};
pub use neuromodulation::{
    NcnSignals, NeuromodulationEngine, NeuromodulatorSnapshot, NeuromodulatorState,
};
pub use orchestrator::{ConsciousnessConfig, ConsciousnessOrchestrator, TickResult};
pub use peer_transport::{PeerMessage, PeerTransport};
pub use prediction_market::{PredictionMarketLedger, PredictionRecord, StrategyPerformance};
pub use somatic::{
    AutobiographicalMemory, EnactiveLoop, FlowState, HomeostaticDrive, SomaticMarker, TheoryOfMind,
};
pub use traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Contradiction,
    ContradictionResolution, PhenomenalState, Priority, Proposal, TemporalNarrative, Verdict,
    VerdictKind, VetoRecord,
};
pub use wisdom::{WisdomAccumulator, WisdomEntry};
