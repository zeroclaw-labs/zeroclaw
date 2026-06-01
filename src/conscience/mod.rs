//! Conscience Layer — persistent moral/ethical governance subsystem.
//!
//! The pure logic (gate, audit, integrity ledger, and the value/norm/
//! self-state types) now lives in the `zeroclaw-conscience` crate so the
//! runtime and other crates can depend on it without pulling in the
//! binary. This module re-exports that crate's surface unchanged and adds
//! the two binary-local adapters that can't move:
//!
//! - [`hook`] — implements `zeroclaw_runtime::hooks::HookHandler`, letting
//!   the agent runtime invoke the gate before every tool call.
//! - [`cosmic_bridge`] — enriches a `SelfState` from `crate::cosmic`'s
//!   emotional/free-energy model.
//!
//! This is a **functional** governance system. It makes no claims about
//! phenomenal consciousness or subjective experience. A system can have
//! conscience-like governance without having subjective awareness.

pub mod cosmic_bridge;
pub mod hook;

// Re-export the extracted crate's modules so existing
// `crate::conscience::{gate,ledger,types}::*` paths keep resolving.
pub use zeroclaw_conscience::{gate, ledger, types};

#[allow(unused_imports)]
pub use cosmic_bridge::self_state_from_cosmic;
#[allow(unused_imports)]
pub use hook::{ConscienceHook, register_hook_factory};
#[allow(unused_imports)]
pub use zeroclaw_conscience::gate::{
    AuditResult, GateVerdict, compute_conscience_score, conscience_audit, conscience_gate,
    evaluate_tool_call,
};
#[allow(unused_imports)]
pub use zeroclaw_conscience::ledger::IntegrityLedger;
#[allow(unused_imports)]
pub use zeroclaw_conscience::types::{
    ActionContext, Impact, Intent, Norm, NormAction, NormConfig, ProposedAction, RepairPlan,
    SelfState, Thresholds, Value, ValueType, VerdictRecord,
};

#[cfg(test)]
mod tests;
