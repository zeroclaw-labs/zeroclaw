//! The single gate-clearing chokepoint (EPIC C, C3).

use anyhow::Result;

use super::ApprovalMode;
use super::decision::{ApprovalDecision, ResolveOutcome};
use super::ledger::{GateEventKind, GateLedgerEntry};
use super::principal::ApprovalPrincipal;
use crate::sop::engine::now_iso8601;
use crate::sop::engine::{GateState, SopEngine};
use crate::sop::types::SopRunStatus;

/// True if `approval_mode` rejects this principal outright: the agent cannot
/// self-satisfy under `OutOfBandRequired`, and an out-of-band principal cannot
/// satisfy under `AgentTool`. Shared by `resolve_gate` and the approval
/// broker's quorum vote-recording path (`ApprovalBroker::resolve`), which must
/// apply this check BEFORE appending a vote - not just at the final
/// `resolve_gate` call that reaches quorum - so a principal `approval_mode`
/// would reject can never durably vote at all, even toward a quorum a
/// different, valid principal later completes.
pub(crate) fn is_rejected_by_approval_mode(
    mode: ApprovalMode,
    principal: &ApprovalPrincipal,
) -> bool {
    match mode {
        ApprovalMode::Both => false,
        ApprovalMode::OutOfBandRequired => !principal.is_out_of_band(),
        ApprovalMode::AgentTool => principal.is_out_of_band(),
    }
}

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
    if is_rejected_by_approval_mode(engine.config().approval_mode, &principal) {
        return Ok(ResolveOutcome::RejectedSelfApproval);
    }

    // 2a. Refuse to resolve an approval while the run's parked snapshot has not
    //     yet been durably persisted (`persist_parked_snapshot_then_release_claim`'s
    //     fail-closed keep, tracked in `claims_pending_persist`). That claim
    //     predates this resolve attempt; reacquiring on top of it would give a
    //     later rollback (on a ledger-append failure) no way to tell "freshly
    //     reacquired by ME, safe to release on failure" from "pre-existing kept
    //     claim, must survive" - releasing it would strand the run BOTH
    //     claimless AND unpersisted. Fail closed here instead: the gate stays
    //     waiting, re-resolvable once a maintenance tick's retry durably
    //     persists the park.
    if matches!(decision, ApprovalDecision::Approve) && engine.is_park_persist_pending(run_id) {
        return Err(anyhow::Error::msg(format!(
            "cannot approve run {run_id}: its parked snapshot is not yet durably persisted (retrying)"
        )));
    }

    // 2b. Pre-flight the fallible transition lookups BEFORE reacquiring the claim
    //     or writing the ledger row (approvals only). `clear_waiting_gate` can still
    //     fail AFTER the audit append if the SOP was removed or its waiting step no
    //     longer resolves (operator edited/removed the SOP while the run sat parked).
    //     If that Err surfaced after step 3, a durable `gate_resolved` row would
    //     exist for a gate that never cleared (metrics counting a phantom approval)
    //     AND the reacquired exec slot would leak. Validating the same lookups here,
    //     under the engine mutex, makes the resolution fail closed with the gate
    //     untouched (still WaitingApproval, re-resolvable) - no claim, no ledger row.
    if matches!(decision, ApprovalDecision::Approve) {
        engine.can_clear_waiting_gate(run_id)?;
    }

    // 2c. Secure the exec claim BEFORE the audit row (approvals only). A run parked
    //     at the gate released its slot; resuming must re-acquire it. Doing this
    //     BEFORE the `gate_resolved` append means a claim-store failure aborts the
    //     resolution WITHOUT writing a false "resolved" audit row (which metrics
    //     would otherwise count as a real approval) and leaves the gate waiting.
    //     It also guarantees the run holds its claim before ANY transition out of
    //     WaitingApproval - including the `Pending` (route-ineligible) branch inside
    //     clear_waiting_gate - so a live non-terminal run is never claimless.
    if matches!(decision, ApprovalDecision::Approve) {
        engine.reacquire_claim_on_resume(run_id)?;
    }

    // 3. Audit FIRST, fail-closed. Durably append the immutable ledger row
    //    (WHO/what/when) BEFORE any gate transition, so a store failure aborts the
    //    resolution and leaves the gate untouched: the gate cannot clear or deny
    //    without its audit-of-record row. (The store ledger is the only audit
    //    source now that the legacy Memory approval audit is gone.)
    if let Err(e) = engine.record_gate_event(GateLedgerEntry {
        run_id: run_id.to_string(),
        step,
        kind: GateEventKind::Resolved,
        decision: Some(decision.clone()),
        principal: principal.clone(),
        ts: now_iso8601(),
    }) {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"run_id": run_id, "error": e.to_string()})),
            "SOP gate resolution aborted: could not persist the audit ledger row (fail-closed)"
        );
        // Roll back the exec claim re-acquired in step 2c: the run stays parked at
        // WaitingApproval, so it must NOT keep occupying an exec slot (which would
        // wrongly defer later triggers). Deny took no claim, so this only matters for
        // Approve.
        if matches!(decision, ApprovalDecision::Approve) {
            engine.release_claim_on_park(run_id);
        }
        return Err(anyhow::Error::msg(format!(
            "failed to persist approval ledger event: {e}"
        )));
    }

    // 4. Apply the decision (only after the audit row is durable).
    let outcome = match decision {
        ApprovalDecision::Approve => {
            let action = match engine.clear_waiting_gate(run_id) {
                Ok(action) => action,
                Err(e) => {
                    // Defensive: step 2b pre-flighted the same lookups under this
                    // lock, so this is unreachable in practice. If the transition
                    // still fails, release the claim reacquired in step 2c so the
                    // run - which stays parked at WaitingApproval - does not leak an
                    // exec slot, then propagate. The gate_resolved row is already
                    // durable here, but the pre-flight guarantees we never reach this
                    // branch for the removed/shrunk-SOP case that would make it false.
                    engine.release_claim_on_park(run_id);
                    return Err(e);
                }
            };
            // Meter the approval at the chokepoint (every principal, exactly once):
            // a `system` principal is a timeout auto-approval, any other a human
            // approval. Keeps the live counters in lockstep with the ledger-sourced
            // `rebuild_from_persistence`.
            engine.record_approval_metric(run_id, principal.is_system());
            ResolveOutcome::Resumed(Box::new(action))
        }
        ApprovalDecision::Deny { reason } => {
            let why = reason.unwrap_or_else(|| format!("denied by {}", principal.actor_label()));
            engine.finish_run(run_id, SopRunStatus::Cancelled, Some(why))?;
            ResolveOutcome::Denied
        }
    };

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::approval::principal::ApprovalPrincipal;
    use crate::sop::engine::SopEngine;
    use crate::sop::store::model::{
        ClaimToken, PersistedRun, ProposalRecord, ProposalStatus, RetentionPolicy, SopEventRecord,
    };
    use crate::sop::store::{InMemoryRunStore, SopRunStore, StoreError};
    use crate::sop::types::{
        Sop, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopStep, SopStepKind,
        SopTrigger, SopTriggerSource,
    };
    use std::sync::Arc;
    use zeroclaw_config::schema::{ApprovalMode, SopConfig};

    /// A store that fails every `append_event` (the gate ledger) but delegates
    /// everything else to a real in-memory store - to prove gate resolution fails
    /// closed when the audit-of-record row cannot be persisted.
    struct FailingAppendStore {
        inner: InMemoryRunStore,
    }
    impl FailingAppendStore {
        fn new() -> Self {
            Self {
                inner: InMemoryRunStore::new(),
            }
        }
    }
    impl SopRunStore for FailingAppendStore {
        fn save_run(&self, run: &PersistedRun) -> Result<(), StoreError> {
            self.inner.save_run(run)
        }
        fn finish_run(&self, run_id: &str, terminal: &PersistedRun) -> Result<(), StoreError> {
            self.inner.finish_run(run_id, terminal)
        }
        fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError> {
            self.inner.load_active_runs()
        }
        fn load_terminal_runs(&self, limit: usize) -> Result<Vec<PersistedRun>, StoreError> {
            self.inner.load_terminal_runs(limit)
        }
        fn load_run(&self, run_id: &str) -> Result<Option<PersistedRun>, StoreError> {
            self.inner.load_run(run_id)
        }
        fn last_terminal_completed_at(&self, sop_name: &str) -> Result<Option<String>, StoreError> {
            self.inner.last_terminal_completed_at(sop_name)
        }
        fn try_claim_run(
            &self,
            run_id: &str,
            sop_name: &str,
            per_sop_cap: usize,
            global_cap: usize,
        ) -> Result<Option<ClaimToken>, StoreError> {
            self.inner
                .try_claim_run(run_id, sop_name, per_sop_cap, global_cap)
        }
        fn renew_claim_for_restore(
            &self,
            run_id: &str,
            sop_name: &str,
        ) -> Result<ClaimToken, StoreError> {
            self.inner.renew_claim_for_restore(run_id, sop_name)
        }
        fn claim_counts(&self, sop_name: &str) -> Result<(usize, usize), StoreError> {
            self.inner.claim_counts(sop_name)
        }
        fn heartbeat_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
            self.inner.heartbeat_claim(token)
        }
        fn release_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
            self.inner.release_claim(token)
        }
        fn expired_claims(&self, now_iso: &str) -> Result<Vec<ClaimToken>, StoreError> {
            self.inner.expired_claims(now_iso)
        }
        // The one method under test: the gate ledger append always fails.
        fn append_event(&self, _ev: &SopEventRecord) -> Result<u64, StoreError> {
            Err(StoreError::Backend("injected append failure".into()))
        }
        fn list_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
            self.inner.list_events(run_id)
        }
        fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError> {
            self.inner.save_proposal(p)
        }
        fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
            self.inner.load_proposal(id)
        }
        fn list_proposals(
            &self,
            status: Option<ProposalStatus>,
        ) -> Result<Vec<ProposalRecord>, StoreError> {
            self.inner.list_proposals(status)
        }
        fn prune(&self, policy: &RetentionPolicy) -> Result<usize, StoreError> {
            self.inner.prune(policy)
        }
        fn health_check(&self) -> bool {
            self.inner.health_check()
        }
        fn backend(&self) -> &'static str {
            "failing-append-test"
        }
    }

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
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "two".into(),
                    body: "b".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::Execute,
                    schema: None,
                    ..SopStep::default()
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
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

    #[test]
    fn store_failure_aborts_resolution_and_leaves_gate_open() {
        // Audit-first, fail-closed (the reviewer's invariant): if the gate ledger
        // row cannot be persisted, resolution must abort and the gate must stay
        // open - the system cannot clear a gate without recording who resolved it.
        let cfg = SopConfig {
            approval_mode: ApprovalMode::Both,
            ..Default::default()
        };
        let mut e = SopEngine::new(cfg).with_store(Arc::new(FailingAppendStore::new()));
        e.set_sops_for_test(vec![supervised_sop("deploy")]);
        let id = start_waiting(&mut e);

        let res = resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(Some("alice".into())),
        );
        assert!(
            res.is_err(),
            "a store append failure must abort the resolution, not swallow it"
        );
        assert!(
            matches!(e.gate_state(&id), GateState::Waiting { .. }),
            "the gate must remain WaitingApproval when its audit row could not be persisted"
        );
    }

    #[test]
    fn approve_fails_closed_when_sop_removed_while_parked() {
        // Regression: the transition `clear_waiting_gate` can still fail AFTER the
        // gate ledger append if the operator removed/shrank the SOP while the run sat
        // parked. Without the step-2b pre-flight, that would leave a durable
        // `gate_resolved` row for a gate that never cleared (a phantom approval in the
        // metrics) AND leak the reacquired exec slot. The pre-flight must make the
        // whole resolution fail closed BEFORE the claim + ledger.
        let cfg = SopConfig {
            approval_mode: ApprovalMode::Both,
            ..Default::default()
        };
        let store = Arc::new(InMemoryRunStore::new());
        let mut e = SopEngine::new(cfg).with_store(store.clone());
        e.set_sops_for_test(vec![supervised_sop("deploy")]);
        let id = start_waiting(&mut e);

        // Operator removes the SOP definition out from under the parked run.
        e.set_sops_for_test(vec![]);

        let res = resolve_gate(
            &mut e,
            &id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(Some("alice".into())),
        );
        assert!(
            res.is_err(),
            "resolution must fail closed when the SOP is gone, not half-commit"
        );

        // No false `gate_resolved` ledger row was written (metrics would otherwise
        // count a phantom approval): the pre-flight aborts BEFORE the append.
        let events = store.list_events(&id).unwrap();
        assert!(
            !events.iter().any(|ev| ev.kind == "gate_resolved"),
            "a removed-SOP resolution must not leave a phantom gate_resolved row: {events:?}"
        );

        // The gate is genuinely still waiting - the approval was not lost.
        assert!(
            matches!(e.gate_state(&id), GateState::Waiting { .. }),
            "the gate must remain WaitingApproval after a failed-closed resolution"
        );

        // The exec slot was not leaked: restore the SOP and a fresh trigger must admit.
        // With max_concurrent=1, a claim leaked by the parked run would defer this.
        e.set_sops_for_test(vec![supervised_sop("deploy")]);
        let action = e.start_run("deploy", manual()).unwrap();
        assert!(
            matches!(action, SopRunAction::WaitApproval { .. }),
            "a fresh run must admit - no phantom exec slot held by the parked run: {action:?}"
        );
    }

    #[test]
    fn timeout_cancel_fails_closed_when_audit_unwritable() {
        // The timeout path is fail-closed too: a cancel must not terminate a run
        // unless its `gate_timed_out` row is durably recorded.
        use crate::sop::types::SopRunStatus;
        use zeroclaw_config::schema::ApprovalTimeoutAction;

        let cfg = SopConfig {
            approval_mode: ApprovalMode::Both,
            approval_timeout_action: ApprovalTimeoutAction::Cancel,
            ..Default::default()
        };
        let mut e = SopEngine::new(cfg).with_store(Arc::new(FailingAppendStore::new()));
        e.set_sops_for_test(vec![supervised_sop("deploy")]);
        let id = start_waiting(&mut e);

        let action = crate::sop::approval::timeout::apply_timeout_action(
            &mut e,
            &id,
            ApprovalTimeoutAction::Cancel,
        );
        assert!(
            action.is_none(),
            "cancel must be skipped when its audit row cannot be persisted"
        );
        assert!(
            !matches!(
                e.get_run(&id).map(|r| &r.status),
                Some(SopRunStatus::Cancelled)
            ),
            "the run must NOT be cancelled without its durable timeout ledger row"
        );
    }
}
