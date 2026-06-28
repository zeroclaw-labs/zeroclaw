//! The append-only approval ledger entry, mapped onto EPIC B's `SopEventRecord`.
//!
//! Replaces the old last-write-wins `sop_approval_{run}_{step}` overwrite: every
//! gate event becomes an immutable, monotonic-seq row via the store's
//! `append_event`, carrying WHO resolved it (the principal) and WHY.

use serde::{Deserialize, Serialize};

use super::decision::ApprovalDecision;
use super::principal::ApprovalPrincipal;
use crate::sop::store::model::SopEventRecord;

/// The kind of gate event recorded in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateEventKind {
    /// A gate opened (a run entered WaitingApproval).
    Requested,
    /// A principal resolved the gate (approve/deny).
    Resolved,
    /// A timeout re-surfaced the gate to the out-of-band approver (fail-closed).
    Escalated,
    /// A timeout terminated the gate (fail-closed cancel).
    TimedOut,
}

impl GateEventKind {
    /// Wire label used as the `SopEventRecord.kind` string.
    pub fn as_str(self) -> &'static str {
        match self {
            GateEventKind::Requested => "gate_requested",
            GateEventKind::Resolved => "gate_resolved",
            GateEventKind::Escalated => "gate_escalated",
            GateEventKind::TimedOut => "gate_timed_out",
        }
    }
}

/// One immutable ledger row per gate event. Becomes a `SopEventRecord` via
/// `into_event_record`, which the store appends (assigning the monotonic seq).
#[derive(Debug, Clone)]
pub struct GateLedgerEntry {
    pub run_id: String,
    pub step: u32,
    pub kind: GateEventKind,
    pub decision: Option<ApprovalDecision>,
    pub principal: ApprovalPrincipal,
    pub ts: String,
}

impl GateLedgerEntry {
    /// Map onto EPIC B's append-only `SopEventRecord`. `seq` is assigned by the
    /// store at `append_event` time (left 0 here). `actor` is the principal's
    /// best-effort identity (falling back to the source label); `reason` carries a
    /// deny reason when present; `payload` records source/channel/step/decision.
    pub fn into_event_record(self) -> SopEventRecord {
        let reason = match &self.decision {
            Some(ApprovalDecision::Deny {
                reason: Some(reason),
            }) => Some(reason.clone()),
            _ => None,
        };
        let decision_label = match &self.decision {
            Some(ApprovalDecision::Approve) => Some("approve"),
            Some(ApprovalDecision::Deny { .. }) => Some("deny"),
            None => None,
        };
        let payload = serde_json::json!({
            "step": self.step,
            "source": self.principal.source_label(),
            "channel": self.principal.channel,
            "decision": decision_label,
        });
        SopEventRecord {
            run_id: self.run_id,
            seq: 0,
            ts: self.ts,
            kind: self.kind.as_str().to_string(),
            actor: Some(self.principal.actor_label()),
            reason,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn into_event_record_carries_who_and_kind() {
        let entry = GateLedgerEntry {
            run_id: "r1".into(),
            step: 2,
            kind: GateEventKind::Resolved,
            decision: Some(ApprovalDecision::Approve),
            principal: ApprovalPrincipal::cli(Some("alice".into())),
            ts: "2026-01-01T00:00:00Z".into(),
        };
        let rec = entry.into_event_record();
        assert_eq!(rec.kind, "gate_resolved");
        assert_eq!(rec.actor.as_deref(), Some("alice"));
        assert_eq!(rec.payload["source"], "cli");
        assert_eq!(rec.payload["decision"], "approve");
        assert_eq!(rec.payload["step"], 2);
        assert!(rec.reason.is_none());
    }

    #[test]
    fn deny_reason_threads_into_reason_field() {
        let entry = GateLedgerEntry {
            run_id: "r1".into(),
            step: 1,
            kind: GateEventKind::Resolved,
            decision: Some(ApprovalDecision::Deny {
                reason: Some("policy".into()),
            }),
            principal: ApprovalPrincipal::system(),
            ts: "t".into(),
        };
        let rec = entry.into_event_record();
        assert_eq!(rec.reason.as_deref(), Some("policy"));
        assert_eq!(rec.actor.as_deref(), Some("system"));
        assert_eq!(rec.payload["decision"], "deny");
    }
}
