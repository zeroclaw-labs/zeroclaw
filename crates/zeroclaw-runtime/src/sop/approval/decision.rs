//! The decision a principal makes on a SOP gate, and the outcome of resolving it.

use serde::{Deserialize, Serialize};

use crate::sop::types::SopRunAction;

/// Approve or deny a waiting SOP gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl ApprovalDecision {
    /// Parse a flat WebSocket approval frame where `decision` is a bare verb
    /// (`approve` / `deny`) and `reason` is an optional sibling field. The wire
    /// shape is flat, not the externally-tagged form serde derives for the enum
    /// itself, so a flat mirror struct carries the serde rules and this is the
    /// one place that bridges the frame to the typed decision.
    #[must_use]
    pub fn from_ws_frame(frame: &serde_json::Value) -> Option<Self> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case", tag = "decision")]
        enum WsFrame {
            Approve,
            Deny {
                #[serde(default)]
                reason: Option<String>,
            },
        }
        match serde_json::from_value::<WsFrame>(frame.clone()).ok()? {
            WsFrame::Approve => Some(Self::Approve),
            WsFrame::Deny { reason } => Some(Self::Deny { reason }),
        }
    }
}

/// What `resolve_gate` did. Returned to the caller (tool / CLI / gateway) so it
/// can report, and so the executor/tick can act on a resumed action.
#[derive(Debug, Clone)]
pub enum ResolveOutcome {
    /// Approved: the next `ExecuteStep` action (the cleared gate).
    Resumed(Box<SopRunAction>),
    /// Denied: the run is Cancelled; no further action.
    Denied,
    /// Idempotent: the run was already resolved within the grace window (a late
    /// timeout racing a human decision). No double ledger row, no double persist.
    AlreadyResolved,
    /// The run is not in `WaitingApproval` (not found / wrong status). Typed, not a panic.
    NotWaiting,
    /// `approval_mode` forbids this principal from clearing the gate (an agent under
    /// `OutOfBandRequired`, or a non-agent under `AgentTool`). The gate stays open.
    RejectedSelfApproval,
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
            ResolveOutcome::AlreadyResolved => "already_resolved",
            ResolveOutcome::NotWaiting => "not_waiting",
            ResolveOutcome::RejectedSelfApproval => "rejected_self_approval",
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
    fn from_ws_frame_parses_the_flat_approval_shape() {
        use serde_json::json;
        assert_eq!(
            ApprovalDecision::from_ws_frame(&json!({"decision": "approve"})),
            Some(ApprovalDecision::Approve)
        );
        assert_eq!(
            ApprovalDecision::from_ws_frame(&json!({"decision": "deny", "reason": "nope"})),
            Some(ApprovalDecision::Deny {
                reason: Some("nope".to_string())
            })
        );
        assert_eq!(
            ApprovalDecision::from_ws_frame(&json!({"decision": "deny"})),
            Some(ApprovalDecision::Deny { reason: None })
        );
        assert_eq!(
            ApprovalDecision::from_ws_frame(&json!({"decision": "garbage"})),
            None
        );
        assert_eq!(ApprovalDecision::from_ws_frame(&json!({})), None);
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
