//! Out-of-band SOP approval plane (EPIC C).
//!
//! The resolution layer on top of EPIC A (the engine singleton) and EPIC B (the
//! durable run store + append-only event log). It provides ONE gate-clearing
//! entry point (`resolve_gate`, added to the engine in a later slice) reachable
//! from four principals - the agent tool, the loopback CLI, the gateway, and the
//! timeout tick - each recorded into B's append-only ledger with a
//! transport-derived principal that a client body can never forge.
//!
//! C0 ships the pure types only (no engine/gateway/CLI wiring): the principal
//! model, the decision/outcome, and the ledger-row mapping onto B's
//! `SopEventRecord`. Subsequent slices add the config mode, the fail-closed
//! timeout, the `resolve_gate` chokepoint, and the out-of-band surfaces.

pub mod decision;
pub mod ledger;
pub mod principal;
pub mod resolve;
pub mod timeout;

pub use decision::{ApprovalDecision, ResolveOutcome};
pub use ledger::{GateEventKind, GateLedgerEntry};
pub use principal::{ApprovalPrincipal, ApprovalSource};

// The approval-policy config enums live in the config crate (one source of truth,
// like `SopRunStoreBackend`); re-exported here so the runtime reads them from the
// approval module.
pub use zeroclaw_config::schema::{ApprovalMode, ApprovalTimeoutAction};

#[cfg(test)]
mod config_tests {
    use super::{ApprovalMode, ApprovalTimeoutAction};
    use zeroclaw_config::schema::SopConfig;

    #[test]
    fn defaults_are_backward_compatible_and_fail_closed() {
        let c = SopConfig::default();
        assert_eq!(c.approval_mode, ApprovalMode::Both);
        assert_eq!(c.approval_timeout_action, ApprovalTimeoutAction::Escalate);
    }

    #[test]
    fn empty_config_loads_defaults_and_unknown_is_rejected() {
        // Existing configs (no approval_* keys) load unchanged.
        let c: SopConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c.approval_mode, ApprovalMode::Both);
        assert_eq!(c.approval_timeout_action, ApprovalTimeoutAction::Escalate);
        // Known values parse.
        let c2: SopConfig = serde_json::from_str(
            r#"{"approval_mode":"out_of_band_required","approval_timeout_action":"cancel"}"#,
        )
        .unwrap();
        assert_eq!(c2.approval_mode, ApprovalMode::OutOfBandRequired);
        assert_eq!(c2.approval_timeout_action, ApprovalTimeoutAction::Cancel);
        // Out-of-set values are rejected at parse time (closed serde enum).
        assert!(serde_json::from_str::<SopConfig>(r#"{"approval_mode":"bogus"}"#).is_err());
    }
}
