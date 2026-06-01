//! Conscience core — persistent moral/ethical governance subsystem.
//!
//! Provides pre-action gating and post-action auditing with:
//! - Value Database (hierarchy + priorities + lexicographic constraints)
//! - Deontic norms (red lines, obligations)
//! - Impact prediction (fast risk/benefit estimate)
//! - Multi-objective arbitration with justification
//! - Integrity Ledger (violations, repairs, credits)
//! - Policy adaptation (threshold/routing updates from outcomes)
//!
//! This is a **functional** governance system. It makes no claims about
//! phenomenal consciousness or subjective experience. A system can have
//! conscience-like governance without having subjective awareness.
//!
//! This crate is intentionally pure: it depends only on `zeroclaw-log`
//! (for the ledger's persistence warnings) and serialization. The
//! adapters that bridge it to the agent runtime (`ConscienceHook`) and
//! the cosmic emotional model (`cosmic_bridge`) live in the binary tree
//! and depend on this crate, not the other way around.

pub mod gate;
pub mod ledger;
pub mod types;

pub use gate::{
    AuditResult, GateVerdict, compute_conscience_score, conscience_audit, conscience_gate,
    evaluate_tool_call,
};
pub use ledger::IntegrityLedger;
pub use types::{
    ActionContext, Impact, Intent, Norm, NormAction, NormConfig, ProposedAction, RepairPlan,
    SelfState, Thresholds, Value, ValueType, VerdictRecord,
};

#[cfg(test)]
mod tests;
