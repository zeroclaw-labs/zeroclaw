//! Fail-closed SOP approval-timeout behavior (EPIC C, C2). [SEC-FLIP]

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
        // Audit-first: don't re-surface unless the escalation row is durably
        // recorded; on a store failure skip this run (it retries next tick).
        ApprovalTimeoutAction::Escalate => {
            let entry = system_entry(engine, run_id, GateEventKind::Escalated);
            if let Err(e) = engine.record_gate_event(entry) {
                log_audit_skip(run_id, "escalate", &e);
                return None;
            }
            engine.restamp_waiting(run_id);
            // EPIC G (Phase 10): if this step's approval policy names a distinct
            // second route, deliver an escalation notice to it (best-effort; the gate
            // stays open regardless). With no policy/route this is a no-op, so the
            // default behavior (re-surface to the same route) is unchanged.
            deliver_escalation_route(engine, run_id);
            None
        }
        // Fail-safe terminal: cancel the run. Audit-first: do not cancel unless
        // the timeout row is durably recorded; on a store failure skip (retries).
        ApprovalTimeoutAction::Cancel => {
            let entry = system_entry(engine, run_id, GateEventKind::TimedOut);
            if let Err(e) = engine.record_gate_event(entry) {
                log_audit_skip(run_id, "cancel", &e);
                return None;
            }
            match engine.finish_run(
                run_id,
                SopRunStatus::Cancelled,
                Some("approval timeout (fail-closed cancel)".to_string()),
            ) {
                Ok(action) => Some(action),
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "error": e.to_string(),
                            })),
                        "SOP timeout: terminal persistence failed; gate left for retry"
                    );
                    None
                }
            }
        }
        // LEGACY, opt-in only: the single path that self-approves on timeout,
        // attributed to the system principal and routed through the chokepoint.
        //
        // This DELIBERATELY calls `resolve_gate` directly, not `resolve_via_broker`:
        // AutoApprove is the operator's explicit fail-OPEN override (default is
        // fail-closed Escalate). It is a `system`-principal auto-resolution on a
        // deadline, not a human approver acting through a policy, so broker
        // membership/quorum do not apply - requiring a group/quorum here would
        // deadlock the very timeout the operator opted into. The audit ledger still
        // records the `system` resolution at the chokepoint.
        ApprovalTimeoutAction::AutoApprove => {
            match engine.resolve_gate(
                run_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::system(),
            ) {
                Ok(ResolveOutcome::Resumed(a)) => Some(*a),
                _ => None,
            }
        }
    }
}

/// Warn that a timeout action was skipped because its audit row could not be
/// persisted (fail-closed: the gate is left for the next tick to retry).
fn log_audit_skip(run_id: &str, action: &str, e: &impl std::fmt::Display) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "run_id": run_id, "action": action, "error": e.to_string()
            })),
        "SOP timeout: skipped, audit ledger append failed; gate left for retry"
    );
}

/// EPIC G (Phase 10): deliver a timeout escalation notice to the second route named
/// by the waiting step's approval policy, if any. Best-effort - a missing policy,
/// missing route, or delivery error never affects the (still-open) gate.
fn deliver_escalation_route(engine: &SopEngine, run_id: &str) {
    let (sop_name, step) = match engine.get_run(run_id) {
        Some(r) => (r.sop_name.clone(), r.current_step),
        None => return,
    };
    let Some(policy_name) = engine.current_step_policy_name(run_id) else {
        return;
    };
    let broker = engine.approval_broker();
    if let Some(route) = broker.escalation_route(engine.approval_config(), &policy_name) {
        broker.deliver_escalation(&route, run_id, &sop_name, step);
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
