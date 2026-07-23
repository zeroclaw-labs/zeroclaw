//! The decision a principal makes on a SOP gate, and the outcome of resolving it.

use serde::{Deserialize, Serialize};

use crate::sop::types::SopRunAction;

/// Approve or deny a waiting SOP gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Approve a deterministic checkpoint WITH an operator amendment: `text`
    /// replaces the step's declared editable field (`- edit:` bullet) in the
    /// piped value before the run resumes. Checkpoint-only; an approval gate
    /// (broker-owned `WaitingApproval`) refuses it fail-closed.
    Amend {
        text: String,
    },
    /// Ask for a re-draft: re-run the checkpoint's predecessor `llm.generate`
    /// step with `guidance` framed as reviewer feedback, then re-present the
    /// gate (revision + 1). Checkpoint-only, same as `Amend`.
    Revise {
        guidance: String,
    },
}

/// What `resolve_gate` did. Returned to the caller (tool / CLI / gateway) so it
/// can report, and so the executor/tick can act on a resumed action.
#[derive(Debug, Clone)]
pub enum ResolveOutcome {
    /// Approved: the next `ExecuteStep` action (the cleared gate).
    Resumed(Box<SopRunAction>),
    /// Denied: the run is Cancelled; no further action.
    Denied,
    /// Revised: the checkpoint's predecessor re-drafted with the operator's
    /// guidance and the gate re-presented (revision + 1). The run stays parked.
    Revised,
    /// Idempotent: the run was already resolved within the grace window (a late
    /// timeout racing a human decision). No double ledger row, no double persist.
    AlreadyResolved,
    /// The run is not in `WaitingApproval` (not found / wrong status). Typed, not a panic.
    NotWaiting,
    /// `approval_mode` forbids this principal from clearing the gate (an agent under
    /// `OutOfBandRequired`, or a non-agent under `AgentTool`). The gate stays open.
    RejectedSelfApproval,
    /// Approved, but every execution slot is full: re-admitting the run would exceed
    /// the SOP's `max_concurrent` or the global `max_concurrent_total`. The gate is
    /// left `WaitingApproval` (untouched, re-resolvable, no ledger row, no claim);
    /// a later resolve or the timeout tick's retry resumes it once a slot frees. This
    /// is routine backpressure, not a rejection - the caps are enforced on resume.
    DeferredAtCapacity,
}

impl ResolveOutcome {
    /// True when the gate was actually cleared (approved + resumed).
    pub fn is_resumed(&self) -> bool {
        matches!(self, ResolveOutcome::Resumed(_))
    }

    /// A stable label for logs / CLI output.
    pub fn label(&self) -> &'static str {
        match self {
            ResolveOutcome::Resumed(_) => "resumed",
            ResolveOutcome::Denied => "denied",
            ResolveOutcome::Revised => "revised",
            ResolveOutcome::AlreadyResolved => "already_resolved",
            ResolveOutcome::NotWaiting => "not_waiting",
            ResolveOutcome::RejectedSelfApproval => "rejected_self_approval",
            ResolveOutcome::DeferredAtCapacity => "deferred_at_capacity",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_serde_round_trips() {
        let a: ApprovalDecision = serde_json::from_str(r#""approve""#).unwrap();
        assert_eq!(a, ApprovalDecision::Approve);
        let d: ApprovalDecision = serde_json::from_str(r#"{"deny":{"reason":"nope"}}"#).unwrap();
        assert_eq!(
            d,
            ApprovalDecision::Deny {
                reason: Some("nope".to_string())
            }
        );
        // Deny without a reason round-trips too.
        let d2: ApprovalDecision = serde_json::from_str(r#"{"deny":{}}"#).unwrap();
        assert_eq!(d2, ApprovalDecision::Deny { reason: None });
    }

    #[test]
    fn outcome_labels_and_is_resumed() {
        assert!(
            ResolveOutcome::Resumed(Box::new(SopRunAction::Completed {
                run_id: "r".into(),
                sop_name: "s".into(),
            }))
            .is_resumed()
        );
        assert!(!ResolveOutcome::Denied.is_resumed());
        assert_eq!(ResolveOutcome::NotWaiting.label(), "not_waiting");
        assert_eq!(
            ResolveOutcome::RejectedSelfApproval.label(),
            "rejected_self_approval"
        );
    }
}
