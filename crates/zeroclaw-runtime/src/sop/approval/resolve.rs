//! The single gate-clearing chokepoint (EPIC C, C3).
//!
//! Every principal - the agent tool, the loopback CLI, the gateway, the timeout
//! tick - funnels through `resolve_gate`. It enforces `approval_mode`, is
//! idempotent (a second resolve in flight is `AlreadyResolved`, no double ledger
//! row), records WHO resolved into B's append-only ledger, and persists the
//! mutated run. `approve_step` keeps its own (unchanged) deterministic-checkpoint
//! path; both share the extracted `clear_waiting_gate` transition body.

use anyhow::Result;

use super::ApprovalMode;
use super::decision::{ApprovalDecision, ResolveOutcome};
use super::ledger::{GateEventKind, GateLedgerEntry};
use super::principal::ApprovalPrincipal;
use crate::sop::engine::now_iso8601;
use crate::sop::engine::{GateState, SopEngine};
use crate::sop::types::SopRunStatus;

/// Resolve a waiting SOP gate. The ONLY place a `WaitingApproval` gate clears.
pub fn resolve_gate(
    engine: &mut SopEngine,
    run_id: &str,
    decision: ApprovalDecision,
    principal: ApprovalPrincipal,
) -> Result<ResolveOutcome> {
    // 1. Locate the run + classify its gate state (idempotency / typed not-found).
    let step = match engine.gate_state(run_id) {
        GateState::Waiting { step } => step,
        GateState::AlreadyResolved => return Ok(ResolveOutcome::AlreadyResolved),
        GateState::NotApplicable => return Ok(ResolveOutcome::NotWaiting),
    };

    // 2. Mode check (the security gate). The agent cannot self-satisfy under
    //    OutOfBandRequired; an out-of-band principal cannot satisfy under AgentTool.
    //    Layered ON TOP of execution_mode/priority/requires_confirmation (those
    //    already decided that the gate exists).
    let mode = engine.config().approval_mode;
    let rejected = match mode {
        ApprovalMode::Both => false,
        ApprovalMode::OutOfBandRequired => !principal.is_out_of_band(),
        ApprovalMode::AgentTool => principal.is_out_of_band(),
    };
    if rejected {
        return Ok(ResolveOutcome::RejectedSelfApproval);
    }

    // 3. Apply the decision.
    let (outcome, kind) = match &decision {
        ApprovalDecision::Approve => {
            let action = engine.clear_waiting_gate(run_id)?;
            // Meter the approval at the chokepoint (every principal, exactly once):
            // a `system` principal is a timeout auto-approval, any other a human
            // approval. Keeps the live counters in lockstep with the ledger-sourced
            // `rebuild_from_persistence`.
            engine.record_approval_metric(run_id, principal.is_system());
            (ResolveOutcome::Resumed(action), GateEventKind::Resolved)
        }
        ApprovalDecision::Deny { reason } => {
            let why = reason
                .clone()
                .unwrap_or_else(|| format!("denied by {}", principal.actor_label()));
            engine.finish_run(run_id, SopRunStatus::Cancelled, Some(why));
            (ResolveOutcome::Denied, GateEventKind::Resolved)
        }
    };

    // 4. Append the immutable ledger row (WHO/what/when) via B's append_event.
    engine.record_gate_event(GateLedgerEntry {
        run_id: run_id.to_string(),
        step,
        kind,
        decision: Some(decision),
        principal,
        ts: now_iso8601(),
    });

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::approval::principal::ApprovalPrincipal;
    use crate::sop::engine::SopEngine;
    use crate::sop::types::{
        Sop, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopStep, SopStepKind,
        SopTrigger, SopTriggerSource,
    };
    use std::sync::Arc;
    use zeroclaw_config::schema::{ApprovalMode, SopConfig};

    fn supervised_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: "t".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "one".into(),
                    body: "b".into(),
                    suggested_tools: vec![],
                    requires_confirmation: true,
                    kind: SopStepKind::Execute,
                    schema: None,
                },
                SopStep {
                    number: 2,
                    title: "two".into(),
                    body: "b".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::Execute,
                    schema: None,
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        }
    }

    fn engine_with(mode: ApprovalMode) -> SopEngine {
        let cfg = SopConfig {
            approval_mode: mode,
            ..Default::default()
        };
        let mut e = SopEngine::new(cfg);
        e.set_sops_for_test(vec![supervised_sop("deploy")]);
        e
    }

    fn manual() -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        }
    }

    // Drive a run to WaitingApproval; returns its run_id.
    fn start_waiting(e: &mut SopEngine) -> String {
        let action = e.start_run("deploy", manual()).unwrap();
        match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        }
    }

    #[test]
    fn not_waiting_for_unknown_run() {
        let mut e = engine_with(ApprovalMode::Both);
        let out = resolve_gate(
            &mut e,
            "nope",
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(None),
        )
        .unwrap();
        assert!(matches!(out, ResolveOutcome::NotWaiting));
    }

    #[test]
    fn approve_resumes_and_idempotent_second_is_already_resolved() {
        let mut e = engine_with(ApprovalMode::Both);
        let id = start_waiting(&mut e);
        let out = resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(Some("alice".into())),
        )
        .unwrap();
        assert!(out.is_resumed(), "first approve resumes");
        // Second resolve of the now-running run is idempotent.
        let again = resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(None),
        )
        .unwrap();
        assert!(matches!(again, ResolveOutcome::AlreadyResolved));
    }

    #[test]
    fn deny_cancels_the_run() {
        let mut e = engine_with(ApprovalMode::Both);
        let id = start_waiting(&mut e);
        let out = resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Deny {
                reason: Some("nope".into()),
            },
            ApprovalPrincipal::cli(None),
        )
        .unwrap();
        assert!(matches!(out, ResolveOutcome::Denied));
    }

    #[test]
    fn out_of_band_required_rejects_agent_keeps_gate_open() {
        let mut e = engine_with(ApprovalMode::OutOfBandRequired);
        let id = start_waiting(&mut e);
        let out = resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::agent("bot"),
        )
        .unwrap();
        assert!(matches!(out, ResolveOutcome::RejectedSelfApproval));
        // The out-of-band principal CAN clear it.
        let cli = resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(None),
        )
        .unwrap();
        assert!(cli.is_resumed());
    }

    #[test]
    fn agent_tool_mode_rejects_out_of_band() {
        let mut e = engine_with(ApprovalMode::AgentTool);
        let id = start_waiting(&mut e);
        let out = resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(None),
        )
        .unwrap();
        assert!(matches!(out, ResolveOutcome::RejectedSelfApproval));
    }

    #[test]
    fn ledger_row_appended_with_principal() {
        let mut e = engine_with(ApprovalMode::Both);
        let id = start_waiting(&mut e);
        let _ = Arc::new(()); // keep imports tidy
        resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(Some("alice".into())),
        )
        .unwrap();
        let events = e.run_events(&id).unwrap();
        let resolved = events
            .iter()
            .find(|ev| ev.kind == "gate_resolved")
            .expect("a gate_resolved ledger row");
        assert_eq!(resolved.actor.as_deref(), Some("alice"));
        assert_eq!(resolved.payload["source"], "cli");
    }

    // Build an engine with a known collector injected, driven to one waiting gate.
    fn engine_metered() -> (SopEngine, Arc<crate::sop::SopMetricsCollector>, String) {
        let collector = Arc::new(crate::sop::SopMetricsCollector::new());
        let cfg = SopConfig {
            approval_mode: ApprovalMode::Both,
            ..Default::default()
        };
        let mut e = SopEngine::new(cfg).with_metrics(Arc::clone(&collector));
        e.set_sops_for_test(vec![supervised_sop("deploy")]);
        let id = start_waiting(&mut e);
        (e, collector, id)
    }

    #[test]
    fn out_of_band_approval_metered_as_human_at_chokepoint() {
        use serde_json::json;
        // The metric is recorded at the chokepoint, so an out-of-band (CLI)
        // approval - not just the agent tool - increments the human counter.
        let (mut e, collector, id) = engine_metered();
        resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(Some("alice".into())),
        )
        .unwrap();
        assert_eq!(
            collector.get_metric_value("sop.human_intervention_count"),
            Some(json!(1u64)),
            "an out-of-band CLI approval is metered as a human approval"
        );
        assert_eq!(
            collector.get_metric_value("sop.timeout_auto_approvals"),
            Some(json!(0u64)),
            "a human approval is not a timeout auto-approval"
        );
    }

    #[test]
    fn system_approval_metered_as_timeout_auto_approve() {
        use serde_json::json;
        // The synthetic `system` principal (the timeout AutoApprove path) is
        // metered as a timeout auto-approval, never a human approval.
        let (mut e, collector, id) = engine_metered();
        resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::system(),
        )
        .unwrap();
        assert_eq!(
            collector.get_metric_value("sop.timeout_auto_approvals"),
            Some(json!(1u64)),
            "a system-principal approval is metered as a timeout auto-approval"
        );
        assert_eq!(
            collector.get_metric_value("sop.human_intervention_count"),
            Some(json!(0u64)),
            "a system approval does not inflate the human counter"
        );
    }
}
