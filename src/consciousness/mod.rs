pub mod agents;
pub mod bus;
pub mod collective;
pub mod dream;
pub mod metacognition;
pub mod orchestrator;
pub mod peer_transport;
pub mod somatic;
pub mod traits;
pub mod wisdom;

pub use bus::{BusMessage, SharedBus};
pub use collective::{CollectiveConsciousness, CollectiveField, PeerState, ResonanceEvent};
pub use dream::{DreamConsolidator, DreamFragment};
pub use metacognition::{
    Adjustment, MetacognitiveEngine, MetacognitiveObservation, MetacognitivePolicy,
};
pub use orchestrator::{ConsciousnessConfig, ConsciousnessOrchestrator, TickResult};
pub use peer_transport::{PeerMessage, PeerTransport};
pub use somatic::{
    AutobiographicalMemory, EnactiveLoop, FlowState, HomeostaticDrive, SomaticMarker, TheoryOfMind,
};
pub use traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Contradiction,
    ContradictionResolution, PhenomenalState, Priority, Proposal, TemporalNarrative, Verdict,
    VerdictKind,
};
pub use wisdom::{WisdomAccumulator, WisdomEntry};
