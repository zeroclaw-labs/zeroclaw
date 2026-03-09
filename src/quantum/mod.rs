pub mod brain;
pub mod circuit;
pub mod gates;
pub mod state;
pub mod traits;

pub use brain::{QuantumBrainEngine, QuantumConsciousnessAgent};
pub use circuit::{CircuitResult, GateOp, QuantumCircuit};
pub use gates::Gate2x2;
pub use state::{QuantumRegister, QuantumState, Qubit};
pub use traits::{
    EntanglementMap, EntanglementPair, ProposalSuperposition, QuantumBrain, QuantumPhaseSpace,
    QuantumProposal,
};
