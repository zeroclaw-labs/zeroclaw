#[allow(unused_imports)]
pub mod brain;
pub mod circuit;
pub mod gates;
pub mod state;
pub mod traits;

#[allow(unused_imports)]
pub use brain::{QuantumBrainEngine, QuantumConsciousnessAgent};
#[allow(unused_imports)]
pub use circuit::{CircuitResult, GateOp, QuantumCircuit};
#[allow(unused_imports)]
pub use gates::Gate2x2;
#[allow(unused_imports)]
pub use state::{QuantumRegister, QuantumState, Qubit};
#[allow(unused_imports)]
pub use traits::{
    EntanglementMap, EntanglementPair, ProposalSuperposition, QuantumBrain, QuantumPhaseSpace,
    QuantumProposal,
};
