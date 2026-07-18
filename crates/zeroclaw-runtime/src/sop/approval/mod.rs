//! Out-of-band SOP approval plane (EPIC C).

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
