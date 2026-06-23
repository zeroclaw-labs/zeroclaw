//! Fail-closed SOP approval-timeout behavior (EPIC C, C2). [SEC-FLIP]
//!
//! The default `Escalate` re-surfaces a timed-out gate to the out-of-band approver
//! and NEVER self-approves. `Cancel` terminates the run (fail-safe). `AutoApprove`
//! is the ONLY path to the legacy fail-open behavior and is opt-in.
//!
//! NOTE: this behavior is correct but DORMANT until something drives
//! `check_approval_timeouts` on a tick (EPIC A's `sop_tick`, not yet in master);
//! today only tests call it. Landing the fail-closed default now means the tick
//! is safe to turn on the moment it exists.

use super::decision::{ApprovalDecision, ResolveOutcome};
use super::ledger::{GateEventKind, GateLedgerEntry};
use super::principal::ApprovalPrincipal;
use crate::sop::engine::{SopEngine, now_iso8601};
use crate::sop::types::{SopRunAction, SopRunStatus};
use zeroclaw_config::schema::ApprovalTimeoutAction;

/// Apply the configured timeout action to a single timed-out `WaitingApproval`
/// run. Returns an action only when the run actually advanced: `Cancel` -> the
/// terminal action; `AutoApprove` -> the resumed action. `Escalate` returns
/// `None` (the gate stays open).
pub fn apply_timeout_action(
    engine: &mut SopEngine,
    run_id: &str,
    action: ApprovalTimeoutAction,
) -> Option<SopRunAction> {
    match action {
        // Default, fail-closed: keep the gate open, reset the clock so it
        // re-surfaces, record the escalation. The run does NOT self-approve.
        ApprovalTimeoutAction::Escalate => {
            let entry = system_entry(engine, run_id, GateEventKind::Escalated);
            engine.restamp_waiting(run_id);
            engine.record_gate_event(entry);
            None
        }
        // Fail-safe terminal: cancel the run.
        ApprovalTimeoutAction::Cancel => {
            let entry = system_entry(engine, run_id, GateEventKind::TimedOut);
            engine.record_gate_event(entry);
            Some(engine.finish_run(
                run_id,
                SopRunStatus::Cancelled,
                Some("approval timeout (fail-closed cancel)".to_string()),
            ))
        }
        // LEGACY, opt-in only: the single path that self-approves on timeout,
        // attributed to the system principal and routed through the chokepoint.
        ApprovalTimeoutAction::AutoApprove => {
            match engine.resolve_gate(
                run_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::system(),
            ) {
                Ok(ResolveOutcome::Resumed(a)) => Some(a),
                _ => None,
            }
        }
    }
}

/// A system-principal ledger entry for the run's current step.
fn system_entry(engine: &SopEngine, run_id: &str, kind: GateEventKind) -> GateLedgerEntry {
    GateLedgerEntry {
        run_id: run_id.to_string(),
        step: engine.run_current_step(run_id),
        kind,
        decision: None,
        principal: ApprovalPrincipal::system(),
        ts: now_iso8601(),
    }
}
