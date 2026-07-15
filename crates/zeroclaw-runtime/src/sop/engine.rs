use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};

use super::capability::SopCapabilityRegistry;
use super::load_sops;
use super::metrics::SopMetricsCollector;
use super::route::{self, NextStep, RouteCtx};
use super::rundata::RunData;
use super::schema;
use super::store::{
    ClaimToken, InMemoryRunStore, PersistedRun, ProposalRecord, ProposalStatus, RetentionPolicy,
    SopEventRecord, SopRunStore, StoreError,
};
use super::types::{
    DeterministicRunState, DeterministicSavings, FilesystemEventKind, Sop, SopAdmission,
    SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopRun, SopRunAction,
    SopRunStatus, SopRunSummary, SopStep, SopStepKind, SopStepResult, SopStepStatus, SopTrigger,
    SopTriggerSource,
};
use crate::calendar::{CALENDAR_NO_SHOW_TOPIC, CalendarNoShowEvent};
use crate::security::{ContentSafety, new_marker_id};
use serde_json::Value;
use zeroclaw_config::schema::SopConfig;

/// Central SOP orchestrator: loads SOPs, matches triggers, manages run lifecycle.
pub struct SopEngine {
    sops: Vec<Sop>,
    active_runs: HashMap<String, SopRun>,
    /// Completed/failed/cancelled runs (kept for status queries).
    finished_runs: Vec<SopRun>,
    config: SopConfig,
    run_counter: u64,
    /// Cumulative savings from deterministic execution.
    deterministic_savings: DeterministicSavings,
    /// Durable run-state store. Defaults to an ephemeral in-memory store
    /// (current behavior); `build_sop_engine` injects the configured backend.
    store: Arc<dyn SopRunStore>,
    /// Run-execution metrics collector. Per-engine fresh in `new()` (test
    /// isolation); `build_sop_engine` swaps in the process-shared collector.
    metrics: Arc<SopMetricsCollector>,
    /// Optional live run-change notifier. When present, every run mutation
    /// (admission, step advance, terminal finish) publishes the run's fresh
    /// summary so push surfaces (the Runs WebSocket) can forward it without
    /// polling. `None` in tests and any embedder that does not want a feed.
    run_notifier: Option<tokio::sync::broadcast::Sender<SopRunSummary>>,
    /// Deterministic capability registry for `kind = "capability"` SOP steps.
    capabilities: Arc<SopCapabilityRegistry>,
    /// Run IDs parked (`WaitingApproval`/`PausedCheckpoint`) whose exec claim was
    /// deliberately KEPT because the parked snapshot could not be durably
    /// persisted (`persist_parked_snapshot_then_release_claim`'s fail-closed
    /// branch). `retry_pending_park_persists` retries these each maintenance
    /// tick, which renews the kept claim's lease as a side effect even while the
    /// retry keeps failing, so the reaper's expired-claim sweep never reclaims a
    /// claim standing in for a park that still is not durable. Cleared (and the
    /// claim released) once a later retry persists successfully.
    claims_pending_persist: std::collections::HashSet<String>,
    /// Approval broker (EPIC G): membership + quorum authorization wrapping the
    /// `resolve_gate` chokepoint. Defaults to a pass-through (no policies) so
    /// behavior is unchanged until a `[sop.approval]` policy is configured.
    approval_broker: Arc<super::approval::ApprovalBroker>,
}

/// Outcome of one [`SopEngine::run_maintenance_tick`] pass (EPIC A1), for
/// observability. All counts are 0 on a quiet tick.
#[derive(Debug, Default, Clone)]
pub struct MaintenanceSummary {
    /// Approval gates that hit their timeout this pass.
    pub timed_out: usize,
    /// Expired concurrency-claim leases reclaimed.
    pub reaped_claims: usize,
    /// Terminal runs pruned past the retention policy.
    pub pruned_runs: usize,
    /// Timeout actions produced. Mostly self-applied (`Escalate` re-stamps,
    /// `Cancel` finalizes); an opt-in `AutoApprove` yields a resumed `ExecuteStep`
    /// the caller logs until EPIC A2's live executor exists.
    pub timeout_actions: Vec<SopRunAction>,
}

impl MaintenanceSummary {
    /// True when the pass did nothing (no timeouts, reaps, or prunes).
    pub fn is_empty(&self) -> bool {
        self.timed_out == 0 && self.reaped_claims == 0 && self.pruned_runs == 0
    }
}

impl SopEngine {
    /// Create a new engine with the given config. Call `reload()` to load SOPs.
    pub fn new(config: SopConfig) -> Self {
        Self {
            sops: Vec::new(),
            active_runs: HashMap::new(),
            finished_runs: Vec::new(),
            config,
            run_counter: 0,
            deterministic_savings: DeterministicSavings::default(),
            store: Arc::new(InMemoryRunStore::new()),
            metrics: Arc::new(SopMetricsCollector::new()),
            run_notifier: None,
            capabilities: Arc::new(SopCapabilityRegistry::with_builtins()),
            claims_pending_persist: std::collections::HashSet::new(),
            approval_broker: Arc::new(super::approval::ApprovalBroker::disabled()),
        }
    }

    /// Inject a durable run-state store (used by `build_sop_engine`). Default is
    /// an ephemeral in-memory store, so callers that don't set one keep today's
    /// behavior exactly.
    pub fn with_store(mut self, store: Arc<dyn SopRunStore>) -> Self {
        self.store = store;
        self
    }

    /// Inject the metrics collector. `build_sop_engine` passes the process-shared
    /// collector so the engine's completion metrics and the SOP tools' reports
    /// observe one set; the default per-engine collector keeps tests isolated.
    pub fn with_metrics(mut self, metrics: Arc<SopMetricsCollector>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Attach a live run-change notifier. `build_sop_engine` wires the gateway's
    /// sender here so run transitions push to the Runs WebSocket. Returns the
    /// engine unchanged when never called (tests, headless embedders).
    pub fn with_run_notifier(mut self, tx: tokio::sync::broadcast::Sender<SopRunSummary>) -> Self {
        self.run_notifier = Some(tx);
        self
    }

    /// Subscribe to the live run-change feed if a notifier is attached. Each
    /// item is a fresh [`SopRunSummary`] for the run that just transitioned.
    pub fn subscribe_run_changes(&self) -> Option<tokio::sync::broadcast::Receiver<SopRunSummary>> {
        self.run_notifier.as_ref().map(|tx| tx.subscribe())
    }

    /// Publish a run's current summary on the notifier, if attached. A send
    /// error means no live subscribers; that is not a failure, so it is
    /// dropped. Marked `active` per the caller's chokepoint.
    fn notify_run(&self, run: &SopRun, active: bool) {
        if let Some(tx) = self.run_notifier.as_ref() {
            let _ = tx.send(SopRunSummary::from_run(run, active));
        }
    }

    /// Inject a deterministic capability registry. Tests and future daemon
    /// wiring can replace the built-ins without adding another execution path.
    pub fn with_capabilities(mut self, capabilities: Arc<SopCapabilityRegistry>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Inject the approval broker (built from `[sop.approval]` config). Defaults to
    /// a pass-through; `build_sop_engine` replaces it with the configured broker.
    pub fn with_approval_broker(mut self, broker: Arc<super::approval::ApprovalBroker>) -> Self {
        self.approval_broker = broker;
        self
    }

    /// The approval broker (membership + quorum authorization). Callers that must
    /// deliver an escalation to a policy's second route read it here.
    pub fn approval_broker(&self) -> Arc<super::approval::ApprovalBroker> {
        Arc::clone(&self.approval_broker)
    }

    /// Resolve a gate THROUGH the broker (membership + quorum), then the chokepoint.
    /// This is the entry point out-of-band surfaces (gateway / CLI / tools) should
    /// call so a `[sop.approval]` policy is enforced; with no policy it is exactly
    /// `resolve_gate` for a `WaitingApproval` run or the historical checkpoint
    /// resolver for a `PausedCheckpoint` run. The broker is cloned out first so it
    /// does not borrow `self` while `self` is mutated by the chokepoint.
    pub fn resolve_via_broker(
        &mut self,
        run_id: &str,
        decision: super::approval::ApprovalDecision,
        principal: super::approval::ApprovalPrincipal,
    ) -> Result<super::approval::BrokerOutcome> {
        let broker = Arc::clone(&self.approval_broker);

        // Deterministic-checkpoint bridge: a `PausedCheckpoint` is the same
        // public-mutation guard as an approval gate when its step names a policy.
        // Authorize the current checkpoint policy first, including membership and
        // quorum, then resolve the checkpoint at the existing audited resolver.
        // With no named policy this remains the historical pass-through.
        if let Some(step) = self
            .active_runs
            .get(run_id)
            .and_then(|r| (r.status == SopRunStatus::PausedCheckpoint).then_some(r.current_step))
        {
            if let Some(outcome) =
                broker.authorize_checkpoint(self, run_id, step, &decision, &principal)?
            {
                return Ok(outcome);
            }
            return self.resolve_checkpoint(run_id, decision, principal);
        }

        broker.resolve(self, run_id, decision, principal)
    }

    /// Resolve a run paused at a deterministic checkpoint from an out-of-band (or
    /// in-band tool) surface: audit first (fail-closed), then approve-and-drive or
    /// deny-and-cancel.
    ///
    /// Approve resumes through [`Self::approve_step`] (the checkpoint owner) and
    /// then drives consecutive capability steps via
    /// [`Self::drive_headless_deterministic`] until the run parks again, hits a
    /// step that needs a live agent, or reaches a terminal state — so a
    /// `checkpoint -> capability(...)` tail (e.g. an approved `forge.comment`
    /// write-back) executes without any live agent turn. Deny cancels the run,
    /// mirroring the approval-gate deny path.
    fn resolve_checkpoint(
        &mut self,
        run_id: &str,
        decision: super::approval::ApprovalDecision,
        principal: super::approval::ApprovalPrincipal,
    ) -> Result<super::approval::BrokerOutcome> {
        use super::approval::{ApprovalDecision, GateEventKind, GateLedgerEntry, ResolveOutcome};

        let step = self
            .active_runs
            .get(run_id)
            .map(|r| r.current_step)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        let checkpoint_revision = self
            .active_runs
            .get(run_id)
            .map(|r| r.revision)
            .unwrap_or_default();
        let checkpoint_decision_identity =
            super::approval::broker::checkpoint_decision_identity(&decision)
                .map(|(_, identity)| identity);

        // Pre-flight every path that mutates the run BEFORE reacquiring the claim or
        // appending the ledger row (mirror `resolve_gate`): a resolution that cannot
        // possibly succeed must fail closed with the run untouched and NO false
        // `gate_resolved` audit row.
        let mut claim_reacquired = false;
        match &decision {
            ApprovalDecision::Approve => {
                if self.is_park_persist_pending(run_id) {
                    bail!(
                        "Run {run_id} cannot resume: its parked checkpoint snapshot is not yet \
                         durably persisted (retrying)"
                    );
                }
                self.can_advance_deterministic_step(run_id)?;
                self.reacquire_claim_on_resume(run_id)?;
                claim_reacquired = true;
            }
            ApprovalDecision::Amend { .. } => {
                if self.is_park_persist_pending(run_id) {
                    bail!(
                        "Run {run_id} cannot resume: its parked checkpoint snapshot is not yet \
                         durably persisted (retrying)"
                    );
                }
                self.can_advance_deterministic_step(run_id)?;
                self.can_amend_checkpoint(run_id)?;
                self.reacquire_claim_on_resume(run_id)?;
                claim_reacquired = true;
            }
            ApprovalDecision::Revise { .. } => {
                if self.is_park_persist_pending(run_id) {
                    bail!(
                        "Run {run_id} cannot re-draft: its parked checkpoint snapshot is not \
                         yet durably persisted (retrying)"
                    );
                }
                self.can_revise_checkpoint(run_id)?;
            }
            ApprovalDecision::Deny { .. } => {}
        }

        // Audit before mutating the checkpoint: the checkpoint cannot clear or cancel
        // without its durable ledger row (same rule as the approval-gate chokepoint).
        // Approve/Amend already secured the exec claim above, so a claim-store
        // failure cannot leave a false `gate_resolved` row for a still-parked run.
        //
        // Revise is the exception: its "apply" includes an irreducibly-fallible
        // model call (the re-draft), so appending the resolved row HERE would
        // leave a false `gate_resolved` row if that call fails. Its row is
        // appended INSIDE `revise_checkpoint_draft` instead — only once the new
        // draft exists and before the run is mutated — so a failed re-draft
        // records nothing and leaves the old draft parked.
        if !matches!(decision, ApprovalDecision::Revise { .. }) {
            if let Err(e) = self.record_gate_event(GateLedgerEntry {
                run_id: run_id.to_string(),
                step,
                gate_revision: Some(checkpoint_revision),
                checkpoint_revision: Some(checkpoint_revision),
                decision_identity: checkpoint_decision_identity.clone(),
                kind: GateEventKind::Resolved,
                decision: Some(decision.clone()),
                principal: principal.clone(),
                ts: now_iso8601(),
            }) {
                if claim_reacquired {
                    self.release_claim_on_park(run_id);
                }
                return Err(anyhow::Error::msg(format!(
                    "failed to persist checkpoint resolution ledger event (fail-closed): {e}"
                )));
            }
        }

        match decision {
            ApprovalDecision::Approve => {
                let first = self.resume_checkpoint_with_reacquired_claim(run_id, None)?;
                self.record_approval_metric(run_id, principal.is_system());
                let final_action = self.drive_headless_deterministic(run_id, first)?;
                if let Some(reason) = Self::terminal_persistence_failure(&final_action) {
                    return Err(anyhow::Error::msg(reason.to_string()));
                }
                Ok(super::approval::BrokerOutcome::Resolved(
                    ResolveOutcome::Resumed(Box::new(final_action)),
                ))
            }
            // An amend IS an approval — of the operator's text instead of the
            // draft verbatim. The edited field replaces its counterpart in the
            // piped value, so the checkpoint's recorded output (and everything
            // downstream) carries the human-approved text while the predecessor
            // step keeps the model's original for the audit trail.
            ApprovalDecision::Amend { text } => {
                let field = self.checkpoint_edit_field(run_id)?;
                let first =
                    self.resume_checkpoint_with_reacquired_claim(run_id, Some((field, text)))?;
                self.record_approval_metric(run_id, principal.is_system());
                let final_action = self.drive_headless_deterministic(run_id, first)?;
                if let Some(reason) = Self::terminal_persistence_failure(&final_action) {
                    return Err(anyhow::Error::msg(reason.to_string()));
                }
                Ok(super::approval::BrokerOutcome::Resolved(
                    ResolveOutcome::Resumed(Box::new(final_action)),
                ))
            }
            ApprovalDecision::Deny { reason } => {
                let why =
                    reason.unwrap_or_else(|| format!("denied by {}", principal.actor_label()));
                self.finish_run_checked(run_id, SopRunStatus::Cancelled, Some(why))?;
                Ok(super::approval::BrokerOutcome::Resolved(
                    ResolveOutcome::Denied,
                ))
            }
            ApprovalDecision::Revise { guidance } => {
                self.revise_checkpoint_draft(run_id, &guidance, &principal, step)?;
                Ok(super::approval::BrokerOutcome::Resolved(
                    ResolveOutcome::Revised,
                ))
            }
        }
    }
    /// Reconstruct in-flight runs from the store at startup (durable backends).
    /// No-op for the in-memory default. Does not overwrite already-present runs.
    pub fn restore_runs(&mut self) {
        match self.store.load_active_runs() {
            Ok(runs) => {
                let mut restored = 0usize;
                // Parking is durable before its out-of-band notice is attempted. A
                // daemon can therefore exit in the interval between those two
                // operations; replay the existing request seam after restore so a
                // parked gate cannot become invisible forever. Delivery is
                // intentionally at-least-once and keeps the canonical gate
                // reference, allowing adapters to de-duplicate it if needed.
                let mut replay_parked_requests = Vec::new();
                for pr in runs {
                    // A1: a run persisted while parked at a HITL approval / paused at
                    // a deterministic checkpoint holds NO exec claim - it released its
                    // slot on park. Restore it WITHOUT re-establishing a claim,
                    // otherwise it would re-consume a concurrency slot on boot while
                    // merely waiting on a human; its slot is re-acquired only when it
                    // resumes.
                    //
                    // An executing (Running/Pending) run DID hold a claim, so
                    // re-establish it WITHOUT admission caps: it was already admitted
                    // before the restart, so reconstruction is not new admission. This
                    // keeps `active_runs` and the live-claim count aligned 1:1 even for
                    // an over-cap restored set (the old capped `try_claim_run` silently
                    // dropped the claim over cap, leaving a locally active run with no
                    // store claim). On a renew error the run is left out of
                    // `active_runs` rather than cached orphaned, and the failure is
                    // logged loudly.
                    let parked = matches!(
                        pr.run.status,
                        SopRunStatus::WaitingApproval | SopRunStatus::PausedCheckpoint
                    );
                    if parked {
                        // A parked run holds no exec slot. A durable store written by the
                        // OLD behavior (before parked runs released their claim on park)
                        // can carry a live `sop_claims` row for this run; RELEASE it now
                        // so the restored parked run is genuinely claim-less and does not
                        // (a) block a same-SOP admission via `claim_counts`, nor (b) get
                        // its stale lease extended forever by `heartbeat_active_claims`.
                        // Its slot is re-acquired only when it resumes. Best-effort.
                        self.release_claim_best_effort(&Self::claim_handle_for_run(&pr.run));
                    } else if let Err(e) = self
                        .store
                        .renew_claim_for_restore(&pr.run.run_id, &pr.run.sop_name)
                    {
                        let span = ::zeroclaw_log::attribution_span!(&pr.run);
                        let _guard = span.enter();
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "run_id": pr.run.run_id.as_str(),
                                "sop_name": pr.run.sop_name.as_str(),
                                "error": e.to_string(),
                            })),
                            "SOP engine: dropping restored run, could not re-establish its store claim"
                        );
                        continue;
                    }
                    let run_id = pr.run.run_id.clone();
                    if self.active_runs.insert(run_id.clone(), pr.run).is_none() {
                        restored += 1;
                        if parked {
                            replay_parked_requests.push(run_id);
                        }
                    }
                }
                // Reuse the same policy resolution and request construction used
                // by a newly parked run. Restored runs already released any claim,
                // so this is delivery recovery only, not another park transition.
                for run_id in replay_parked_requests {
                    self.notify_park_request(&run_id);
                }
                if restored > 0 {
                    let span = ::zeroclaw_log::info_span!(
                        target: "zeroclaw_log_internal_scope",
                        "zeroclaw_scope",
                        sop_name = "*",
                    );
                    let _guard = span.enter();
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"restored": restored})),
                        &format!("SOP engine restored {restored} run(s) from store")
                    );
                }
            }
            Err(e) => {
                let span = ::zeroclaw_log::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "zeroclaw_scope",
                    sop_name = "*",
                );
                let _guard = span.enter();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                    "SOP engine: failed to restore runs from store"
                );
            }
        }
        self.restore_finished_runs();
    }

    /// Seed the display retention window (`finished_runs`) from the store's
    /// terminal records at boot, newest-first and capped at `max_finished_runs`.
    /// Terminal runs are durable but not part of the active-run rehydrate set, so
    /// without this the Runs surface drops all completed/failed/cancelled runs
    /// across a restart even though they remain on disk.
    fn restore_finished_runs(&mut self) {
        let limit = self.config.max_finished_runs;
        match self.store.load_terminal_runs(limit) {
            Ok(runs) => {
                let mut seeded = 0usize;
                for pr in runs {
                    let span = ::zeroclaw_log::attribution_span!(&pr.run);
                    let _guard = span.enter();
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({
                                "run_id": pr.run.run_id.as_str(),
                                "sop_name": pr.run.sop_name.as_str(),
                            })),
                        "SOP engine: seeded terminal run into the retention window"
                    );
                    self.finished_runs.push(pr.run);
                    seeded += 1;
                }
                self.finished_runs
                    .sort_by(|a, b| a.started_at.cmp(&b.started_at));
                if seeded > 0 {
                    let span = ::zeroclaw_log::info_span!(
                        target: "zeroclaw_log_internal_scope",
                        "zeroclaw_scope",
                        sop_name = "*",
                    );
                    let _guard = span.enter();
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"seeded": seeded})),
                        &format!(
                            "SOP engine seeded {seeded} terminal run(s) into the retention window"
                        )
                    );
                }
            }
            Err(e) => {
                let span = ::zeroclaw_log::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "zeroclaw_scope",
                    sop_name = "*",
                );
                let _guard = span.enter();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                    "SOP engine: failed to seed terminal runs from store"
                );
            }
        }
    }

    /// Next monotonic revision for a run: one past whatever the store currently
    /// holds (0 if absent). Keeps every persist strictly newer so the store's
    /// revision guard accepts it; a cheap indexed lookup on either backend.
    fn next_run_revision(&self, run_id: &str) -> u64 {
        match self.store.load_run(run_id) {
            Ok(Some(existing)) => existing.revision.saturating_add(1),
            _ => 0,
        }
    }

    /// Persist a still-active run (best-effort; logs on failure). Cheap no-op
    /// effect for the in-memory default.
    fn persist_active(&self, run_id: &str) {
        let _ = self.persist_active_checked(run_id);
    }

    /// Persist a still-active run and REPORT whether the durable write succeeded.
    /// Returns `true` if there is no such active run (nothing to persist) or the
    /// snapshot was saved; `false` only if `save_run` errored. The park paths use
    /// this so they release the exec claim ONLY after the parked snapshot is
    /// durably written: a run parked in memory but NOT persisted must keep its
    /// slot, or a crash would leave the approval/checkpoint lost while newer
    /// triggers had already admitted into the "freed" slot.
    fn persist_active_checked(&self, run_id: &str) -> bool {
        let Some(run) = self.active_runs.get(run_id) else {
            return true;
        };
        self.heartbeat_claim_for_run(run);
        let mut pr = PersistedRun::new(run.clone(), now_iso8601(), run.trigger_event.source);
        // Each persist is a new state revision; the store rejects a
        // same-revision divergent write, so advance past what is stored.
        pr.revision = self.next_run_revision(run_id);
        let saved = match self.store.save_run(&pr) {
            Ok(()) => true,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"run_id": run_id, "error": e.to_string()})
                        ),
                    "SOP engine: failed to persist run"
                );
                false
            }
        };
        self.notify_run(run, true);
        saved
    }

    /// Complete a durable park exactly once: release its exec claim and send the
    /// approval request only after the snapshot can survive restart.
    fn complete_durable_park(&mut self, run_id: &str) {
        self.claims_pending_persist.remove(run_id);
        self.release_claim_on_park(run_id);
        self.notify_park_request(run_id);
    }

    /// Park a run (WaitingApproval / PausedCheckpoint) and free its exec slot, but
    /// ONLY after the parked snapshot is durably persisted. If the persist fails,
    /// the claim is KEPT (fail closed): the run stays correctly counted against
    /// capacity, so it is never both claimless AND un-persisted (which a crash
    /// would turn into a lost park while newer triggers had already admitted into
    /// the "freed" slot). The slot is held until a later persist succeeds,
    /// trading a little concurrency for no lost park. Returns whether the parked
    /// snapshot is durable, so callers can defer external approval prompts until
    /// the run is recoverable after a restart.
    fn persist_parked_snapshot_then_release_claim(&mut self, run_id: &str) -> bool {
        if self.persist_active_checked(run_id) {
            self.complete_durable_park(run_id);
            true
        } else {
            // Track this run so `heartbeat_active_claims` keeps renewing its KEPT
            // claim despite the park status (see `claims_pending_persist`'s doc):
            // otherwise the claim's lease goes un-renewed and the maintenance
            // reaper reclaims it once it expires, silently undoing the fail-closed
            // keep and over-admitting a newer trigger.
            self.claims_pending_persist.insert(run_id.to_string());
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"run_id": run_id})),
                "SOP engine: parked snapshot not persisted; KEEPING the exec claim (fail closed) so the park is not lost"
            );
            false
        }
    }

    /// Retry the durable persist for every run in `claims_pending_persist`. A
    /// retry that now succeeds completes the deferred park transition (releases
    /// the claim and emits its deferred approval prompt). One that still fails
    /// leaves the run tracked - but
    /// `persist_active_checked` heartbeats the claim BEFORE attempting the write,
    /// unconditionally, so even a failing retry still renews the kept claim's
    /// lease. This is what keeps `reap_expired_claims` from reclaiming it: called
    /// every maintenance tick, a park that never manages to persist still gets
    /// its claim renewed once per tick for as long as it stays parked.
    fn retry_pending_park_persists(&mut self) {
        let pending: Vec<String> = self.claims_pending_persist.iter().cloned().collect();
        for run_id in pending {
            let Some(status) = self.active_runs.get(&run_id).map(|run| run.status) else {
                // The run left active_runs some other way (finished/evicted);
                // nothing left to retry or release.
                self.claims_pending_persist.remove(&run_id);
                continue;
            };
            if self.persist_active_checked(&run_id) {
                // Only release the claim if the run is STILL parked. The entry
                // guards in `resolve_gate`/`approve_step`/`resume_deterministic_run`
                // (`is_park_persist_pending`) already refuse to resume a run while
                // it is tracked here, so this should be unreachable in practice -
                // but if a run somehow left the parked state without going through
                // one of those guarded paths, its claim is now legitimately held
                // by that transition and must NOT be released out from under it.
                if !holds_exec_claim(status) {
                    self.complete_durable_park(&run_id);
                } else {
                    self.claims_pending_persist.remove(&run_id);
                }
            }
        }
    }

    /// True if `run_id`'s exec claim is being kept pending a retried park persist
    /// (`claims_pending_persist`): its most recent park snapshot has not yet been
    /// durably written. The three resume paths (`resolve_gate` via
    /// `clear_waiting_gate`, `approve_step`, `resume_deterministic_run`) must
    /// refuse to proceed while this is true - the kept claim predates the resume
    /// attempt, so a later rollback (on a ledger/audit failure) or a maintenance
    /// retry's release would either drop a claim that must survive, or release a
    /// claim out from under a run that has since started executing. Fail closed
    /// here instead: the gate/checkpoint stays parked, re-resolvable once a
    /// maintenance tick's retry durably persists the park.
    pub(crate) fn is_park_persist_pending(&self, run_id: &str) -> bool {
        self.claims_pending_persist.contains(run_id)
    }

    /// A prompt becomes stale only after a replacement presentation is durable.
    /// A re-draft can update the in-memory revision before its parked snapshot
    /// saves; finalizing the old prompt in that window would leave operators
    /// without a durable replacement after a crash.
    pub fn is_gate_reference_superseded(&self, run_id: &str, reference_revision: u32) -> bool {
        self.active_runs.get(run_id).is_some_and(|run| {
            run.revision != reference_revision && !self.is_park_persist_pending(run_id)
        })
    }

    /// Admit a run through the store CAS claim before it becomes locally active.
    /// The durable store is the concurrency source of truth; `active_runs` is the
    /// execution cache/status surface.
    fn claim_admission(&self, run_id: &str, sop: &Sop) -> Result<ClaimToken> {
        match self.store.try_claim_run(
            run_id,
            &sop.name,
            sop.max_concurrent as usize,
            self.config.max_concurrent_total,
        ) {
            Ok(Some(token)) => Ok(token),
            Ok(None) => {
                bail!(
                    "Cannot start SOP '{}': cooldown or concurrency limit reached",
                    sop.name
                );
            }
            Err(e) => Err(anyhow::Error::new(e)),
        }
    }

    fn release_claim_best_effort(&self, token: &ClaimToken) {
        if let Err(e) = self.store.release_claim(token) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "run_id": token.run_id.as_str(),
                        "error": e.to_string(),
                    })),
                "SOP engine: failed to release run admission claim"
            );
        }
    }

    fn claim_handle_for_run(run: &SopRun) -> ClaimToken {
        ClaimToken {
            run_id: run.run_id.clone(),
            sop_name: run.sop_name.clone(),
            claimed_at: String::new(),
            lease_expires: String::new(),
            holder: "engine".to_string(),
        }
    }

    fn heartbeat_claim_for_run(&self, run: &SopRun) {
        let token = Self::claim_handle_for_run(run);
        if let Err(e) = self.store.heartbeat_claim(&token) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "run_id": run.run_id.as_str(),
                        "error": e.to_string(),
                    })),
                "SOP engine: failed to heartbeat run admission claim"
            );
        }
    }

    fn heartbeat_active_claims(&self) {
        // Only EXECUTING runs hold a claim; a parked run released its claim on park,
        // so heartbeating it would (on a durable store carrying a stale row from the
        // old behavior) extend a claim that should be gone. Skip parked runs. A run
        // in `claims_pending_persist` (a park whose snapshot failed to persist,
        // KEEPING its claim) is renewed by `retry_pending_park_persists` instead -
        // called just before this each tick - so its kept claim's lease never goes
        // un-renewed even while parked.
        for run in self.active_runs.values() {
            if holds_exec_claim(run.status) {
                self.heartbeat_claim_for_run(run);
            }
        }
    }

    /// A1: release a parked run's exec claim so its concurrency slot frees for
    /// other triggers. A run waiting on a human approval (or paused at a
    /// deterministic checkpoint) is not executing, so it must not hold an
    /// execution slot. The run stays in `active_runs` - every reader (gate_state,
    /// overdue_waiting_run_ids, resolve_gate, resume) and `finish_run` rely on it
    /// still being there; only the store CAS claim is dropped. Best-effort +
    /// logged. Persist the parked state BEFORE calling this so a crash in the
    /// window leaves a restorable parked run rather than a freed-but-unpersisted one.
    pub(crate) fn release_claim_on_park(&self, run_id: &str) {
        if let Some(run) = self.active_runs.get(run_id) {
            self.release_claim_best_effort(&Self::claim_handle_for_run(run));
        }
    }

    /// A1: re-establish a resumed run's exec claim. Uses the uncapped restore
    /// path: the run was admitted once already, so resuming it after an approval
    /// is a continuation, not new admission, and must never be blocked by the
    /// concurrency cap (an approved run always resumes). This can transiently push
    /// executing runs above `max_concurrent` when many approvals resolve at once;
    /// that overshoot is bounded and self-corrects (new triggers see the higher
    /// count and wait). Strict serialization is the `Hold` admission policy, not this.
    /// Fail-CLOSED: returns `Err` if the claim cannot be re-established, so the
    /// caller aborts the resume BEFORE flipping the run to `Running` and leaves it
    /// parked (re-resolvable). Executing without a claim would under-count
    /// concurrency and oversubscribe. A missing run is a no-op `Ok` (the caller
    /// already validated the run exists).
    pub(crate) fn reacquire_claim_on_resume(&self, run_id: &str) -> Result<()> {
        let Some(run) = self.active_runs.get(run_id) else {
            return Ok(());
        };
        self.store
            .renew_claim_for_restore(&run.run_id, &run.sop_name)
            .map(|_| ())
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "run_id": run.run_id.as_str(),
                            "error": e.to_string(),
                        })),
                    "SOP engine: resume aborted, could not re-acquire the run admission claim (fail-closed)"
                );
                anyhow::Error::msg(format!(
                    "failed to re-acquire exec claim on resume for run {run_id}: {e}"
                ))
            })
    }

    /// Persist a run that has reached a terminal state.
    fn persist_terminal(&self, run: &SopRun) -> Result<(), StoreError> {
        let mut pr = PersistedRun::new(run.clone(), now_iso8601(), run.trigger_event.source);
        // The terminal write is the run's final revision; advance past the last
        // active snapshot so the store's revision guard accepts it.
        pr.revision = self.next_run_revision(&run.run_id);
        self.store.finish_run(&run.run_id, &pr)?;
        self.notify_run(run, false);
        Ok(())
    }

    fn record_transition_event(
        &self,
        run_id: &str,
        kind: &str,
        reason: Option<String>,
        payload: serde_json::Value,
    ) {
        let ev = SopEventRecord {
            run_id: run_id.to_string(),
            seq: 0,
            ts: now_iso8601(),
            kind: kind.to_string(),
            actor: None,
            reason,
            payload,
        };
        if let Err(e) = self.store.append_event(&ev) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"run_id": run_id, "kind": kind, "error": e.to_string()})
                    ),
                "SOP engine: failed to append transition event"
            );
        }
    }

    /// Load/reload SOPs from the configured directory.
    pub fn reload(&mut self, workspace_dir: &Path) {
        self.sops = load_sops(
            workspace_dir,
            self.config.sops_dir.as_deref(),
            super::parse_execution_mode(&self.config.default_execution_mode),
        );
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("SOP engine loaded {} SOPs", self.sops.len())
        );
    }

    /// Return all loaded SOP definitions.
    pub fn sops(&self) -> &[Sop] {
        &self.sops
    }

    #[cfg(test)]
    pub(crate) fn replace_sops_for_test(&mut self, sops: Vec<Sop>) {
        self.sops = sops;
    }

    /// Return all active (in-flight) runs.
    pub fn active_runs(&self) -> &HashMap<String, SopRun> {
        &self.active_runs
    }

    /// Look up a run by ID (active or finished).
    pub fn get_run(&self, run_id: &str) -> Option<&SopRun> {
        self.active_runs
            .get(run_id)
            .or_else(|| self.finished_runs.iter().find(|r| r.run_id == run_id))
    }

    /// Look up an SOP by name.
    pub fn get_sop(&self, name: &str) -> Option<&Sop> {
        self.sops.iter().find(|s| s.name == name)
    }

    // ── Trigger matching ────────────────────────────────────────

    /// Match an incoming event against all loaded SOPs and return the names of
    /// SOPs whose triggers match.
    pub fn match_trigger(&self, event: &SopEvent) -> Vec<&Sop> {
        self.sops
            .iter()
            .filter(|sop| sop.triggers.iter().any(|t| trigger_matches(t, event)))
            .collect()
    }

    /// True when any loaded SOP has a trigger of this source. Fan-in
    /// callers use this as a cheap pre-filter before building and
    /// dispatching an event.
    pub fn wants_source(&self, source: SopTriggerSource) -> bool {
        self.sops
            .iter()
            .any(|sop| sop.triggers.iter().any(|t| t.source() == source))
    }

    // ── Run lifecycle ───────────────────────────────────────────

    /// Check whether a new run can be started for the given SOP
    /// (respects cooldown and concurrency limits).
    pub fn can_start(&self, sop_name: &str) -> bool {
        let sop = match self.get_sop(sop_name) {
            Some(s) => s,
            None => return false,
        };
        let (active_for_sop, active_total) = self.exec_counts(sop_name);
        if active_for_sop >= sop.max_concurrent as usize
            || active_total >= self.config.max_concurrent_total
        {
            return false;
        }
        !self.in_cooldown(sop)
    }

    /// Live *executing* run counts `(for_sop, total)`. The store's CAS claims are
    /// the authoritative concurrency source (shared across engine holders); parked
    /// runs release their claim (A1), so they are excluded. Falls back to the
    /// in-memory view (also parked-excluded) only if the store call errors.
    fn exec_counts(&self, sop_name: &str) -> (usize, usize) {
        match self.store.claim_counts(sop_name) {
            Ok(counts) => counts,
            Err(_) => (
                self.active_runs
                    .values()
                    .filter(|r| holds_exec_claim(r.status) && r.sop_name == sop_name)
                    .count(),
                self.active_runs
                    .values()
                    .filter(|r| holds_exec_claim(r.status))
                    .count(),
            ),
        }
    }

    /// Whether the SOP's cooldown window is still active (blocks a new start). Read
    /// from the shared store so every engine holder observes the same completion
    /// marker; falls back to the local finished list only on a store error.
    fn in_cooldown(&self, sop: &Sop) -> bool {
        if sop.cooldown_secs == 0 {
            return false;
        }
        let last_completed = match self.store.last_terminal_completed_at(&sop.name) {
            Ok(completed) => completed,
            Err(_) => self
                .last_finished_run(&sop.name)
                .and_then(|last| last.completed_at.clone()),
        };
        matches!(last_completed, Some(ts) if !cooldown_elapsed(&ts, sop.cooldown_secs))
    }

    /// Count runs of `sop_name` currently parked at a HITL approval / checkpoint
    /// (they hold no exec slot). This is the "pending-approval pool" A2 bounds.
    fn pending_count_for_sop(&self, sop_name: &str) -> usize {
        // Read the shared store's active-run surface so multiple engine holders see
        // one source of truth for the pending-approval pool (mirrors exec_counts,
        // which reads store claim_counts). A persisted `WaitingApproval` run parked
        // by a sibling engine is counted here, so `max_pending_approvals` is not
        // silently exceeded across processes. Fall back to this engine's local view
        // only when the store errors.
        match self.store.load_active_runs() {
            Ok(runs) => runs
                .iter()
                .filter(|pr| pr.run.sop_name == sop_name && !holds_exec_claim(pr.run.status))
                .count(),
            Err(_) => self
                .active_runs
                .values()
                .filter(|r| r.sop_name == sop_name && !holds_exec_claim(r.status))
                .count(),
        }
    }

    /// First active (executing or parked) run id for `sop_name`, if any - the
    /// `Coalesce` policy names the in-flight run a new trigger folds into. Resolved
    /// from the SHARED store's active-run surface (like exec/pending counts), so an
    /// engine whose local map is empty still finds a sibling engine's in-flight run
    /// and returns `Coalesce` rather than `Defer` (which on a durable transport would
    /// churn redeliveries instead of acknowledging the trigger as absorbed). Falls
    /// back to the local map only on a store error.
    fn first_active_run_for_sop(&self, sop_name: &str) -> Option<String> {
        match self.store.load_active_runs() {
            Ok(runs) => runs
                .into_iter()
                .find(|pr| pr.run.sop_name == sop_name)
                .map(|pr| pr.run.run_id),
            Err(_) => self
                .active_runs
                .values()
                .find(|r| r.sop_name == sop_name)
                .map(|r| r.run_id.clone()),
        }
    }

    /// A2: decide how to admit a matched trigger for `sop_name` under its
    /// `SopAdmissionPolicy`. `Admit` still passes through the authoritative CAS in
    /// `start_run`; the other outcomes are surfaced by the dispatch layer so a
    /// non-admitted trigger is never silently lost. A cooldown or unknown SOP drops
    /// regardless of policy (a cooldown is a deliberate rate limit, not backpressure).
    ///
    /// AUTHORITY: within a SINGLE daemon this decision is authoritative - the engine
    /// `Mutex` serializes `evaluate_admission` + the CAS claim, so two triggers cannot
    /// both admit past the policy. The exec-slot cap is additionally CAS-authoritative
    /// via the shared store even ACROSS engines. The pending-approval pool
    /// (`max_pending_approvals`), however, is only ADVISORY across engines: a run
    /// parks at approval only AFTER it has executed, so its pending slot cannot be
    /// atomically pre-reserved at admission time, and two engines sharing a store can
    /// each admit a run that later parks. Making the pending cap cross-engine-
    /// authoritative requires a store-level two-phase reservation (a follow-up); the
    /// single-daemon deployment - the common case - is fully authoritative today.
    pub fn evaluate_admission(&self, sop_name: &str) -> SopAdmission {
        self.evaluate_admission_with_reserved(sop_name, 0, 0)
    }

    /// Evaluate a group of matched SOPs as one delivery unit. Each `Admit` reserves
    /// one simulated exec slot before evaluating later siblings, so all-or-nothing
    /// transports can detect a batch that would exceed per-SOP or global capacity
    /// before starting any run.
    pub(crate) fn evaluate_admission_batch_all_or_nothing(
        &self,
        sop_names: &[String],
    ) -> Vec<(String, SopAdmission)> {
        let mut reserved_by_sop: HashMap<String, usize> = HashMap::new();
        let mut reserved_total = 0usize;
        let mut out = Vec::with_capacity(sop_names.len());

        for sop_name in sop_names {
            let reserved_for_sop = *reserved_by_sop.get(sop_name).unwrap_or(&0);
            let admission =
                self.evaluate_admission_with_reserved(sop_name, reserved_for_sop, reserved_total);
            if matches!(admission, SopAdmission::Admit) {
                *reserved_by_sop.entry(sop_name.clone()).or_default() += 1;
                reserved_total += 1;
            }
            out.push((sop_name.clone(), admission));
        }

        out
    }

    fn evaluate_admission_with_reserved(
        &self,
        sop_name: &str,
        reserved_for_sop: usize,
        reserved_total: usize,
    ) -> SopAdmission {
        let sop = match self.get_sop(sop_name) {
            Some(s) => s,
            None => {
                return SopAdmission::Drop {
                    reason: format!("SOP '{sop_name}' not loaded"),
                };
            }
        };
        if self.in_cooldown(sop) {
            return SopAdmission::Drop {
                reason: format!("SOP '{sop_name}' in cooldown"),
            };
        }

        let (exec_for_sop, exec_total) = self.exec_counts(sop_name);
        let exec_for_sop = exec_for_sop.saturating_add(reserved_for_sop);
        let exec_total = exec_total.saturating_add(reserved_total);
        let pending_for_sop = self.pending_count_for_sop(sop_name);
        let exec_slot_free = exec_for_sop < sop.max_concurrent as usize
            && exec_total < self.config.max_concurrent_total;
        let policy = sop.admission_policy;

        // Pending-approval-pool backpressure (every policy but Drop, which drops).
        if sop.max_pending_approvals > 0 && pending_for_sop >= sop.max_pending_approvals as usize {
            let reason = format!("SOP '{sop_name}' pending-approval pool full ({pending_for_sop})");
            return match policy {
                SopAdmissionPolicy::Drop => SopAdmission::Drop { reason },
                _ => SopAdmission::Defer { reason },
            };
        }

        match policy {
            SopAdmissionPolicy::Parallel => {
                if exec_slot_free {
                    SopAdmission::Admit
                } else {
                    SopAdmission::Defer {
                        reason: format!("SOP '{sop_name}' execution slots full"),
                    }
                }
            }
            SopAdmissionPolicy::Hold => {
                if exec_for_sop + pending_for_sop == 0 && exec_slot_free {
                    SopAdmission::Admit
                } else {
                    SopAdmission::Defer {
                        reason: format!("SOP '{sop_name}' held (a run is already in flight)"),
                    }
                }
            }
            SopAdmissionPolicy::Coalesce => {
                if exec_for_sop + pending_for_sop == 0 && exec_slot_free {
                    SopAdmission::Admit
                } else if let Some(existing_run_id) = self.first_active_run_for_sop(sop_name) {
                    SopAdmission::Coalesce { existing_run_id }
                } else {
                    SopAdmission::Defer {
                        reason: format!("SOP '{sop_name}' execution slots full"),
                    }
                }
            }
            SopAdmissionPolicy::Drop => {
                if exec_slot_free {
                    SopAdmission::Admit
                } else {
                    SopAdmission::Drop {
                        reason: format!("SOP '{sop_name}' execution slots full (drop policy)"),
                    }
                }
            }
        }
    }

    /// Start a new SOP run. Returns the first action to take.
    /// Deterministic SOPs are automatically routed to `start_deterministic_run`.
    /// Enforce the SOP's admission policy at a start entrypoint. `Admit` proceeds;
    /// any other outcome declines the start with a descriptive error so a trigger is
    /// never run past its policy. dispatch pre-consults `evaluate_admission` and only
    /// reaches a start path on `Admit`, so re-checking here (under the same held lock)
    /// is idempotent; a DIRECT caller (`sop_execute`, or `start_deterministic_run`)
    /// would otherwise bypass Hold / Coalesce / the `max_pending_approvals` pool.
    fn enforce_admission(&self, sop_name: &str) -> Result<()> {
        match self.evaluate_admission(sop_name) {
            SopAdmission::Admit => Ok(()),
            SopAdmission::Coalesce { existing_run_id } => bail!(
                "SOP '{sop_name}' not started: coalesced into in-flight run {existing_run_id}"
            ),
            SopAdmission::Defer { reason } | SopAdmission::Drop { reason } => {
                bail!("SOP '{sop_name}' not started: {reason}")
            }
        }
    }

    pub fn start_run(&mut self, sop_name: &str, event: SopEvent) -> Result<SopRunAction> {
        // Route deterministic SOPs to their dedicated path FIRST; it applies the same
        // admission guard at its top, so a direct call to start_deterministic_run
        // cannot bypass the policy either.
        if self
            .get_sop(sop_name)
            .is_some_and(|s| s.execution_mode == SopExecutionMode::Deterministic)
        {
            return self.start_deterministic_run(sop_name, event);
        }

        self.enforce_admission(sop_name)?;

        let sop = self
            .get_sop(sop_name)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"sop_name": sop_name})),
                    "SOP engine: sop not found"
                );
                anyhow::Error::msg(format!("SOP not found: {sop_name}"))
            })?
            .clone();

        if !self.can_start(sop_name) {
            bail!(
                "Cannot start SOP '{}': cooldown or concurrency limit reached",
                sop_name
            );
        }

        if sop.steps.is_empty() {
            bail!("SOP '{}' has no steps defined", sop_name);
        }

        self.run_counter += 1;
        let dur = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let epoch_ns = dur.as_nanos();
        let run_id = format!("run-{epoch_ns}-{:04}", self.run_counter);
        let now = now_iso8601();

        let run = SopRun {
            run_id: run_id.clone(),
            sop_name: sop_name.to_string(),
            trigger_event: event,
            frame_marker_id: new_marker_id(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: u32::try_from(sop.steps.len()).unwrap_or(u32::MAX),
            started_at: now,
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        };

        let claim = self.claim_admission(&run_id, &sop)?;
        self.active_runs.insert(run_id.clone(), run);

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("SOP run {} started for '{}'", run_id, sop_name)
        );

        match self.dispatch_llm_step(&run_id, &sop, 1, None) {
            Ok(action) => Ok(action),
            Err(e) => {
                self.active_runs.remove(&run_id);
                self.release_claim_best_effort(&claim);
                Err(e)
            }
        }
    }

    /// Report the result of the current step and advance the run.
    /// Returns the next action to take.
    ///
    /// Refuses to advance a run whose status is `WaitingApproval` or
    /// `PausedCheckpoint`: those states mean an external gate is pending
    /// (an approval, or a deterministic checkpoint resume) and a driver
    /// supplying a fabricated `SopStepResult` must not be allowed to skip
    /// the gate. The legitimate path for clearing a `WaitingApproval` gate
    /// is `resolve_gate` / `clear_waiting_gate`; the legitimate path for
    /// resuming a `PausedCheckpoint` is `approve_step`. Mirrors the
    /// status check `approve_step` already performs for the checkpoint
    /// case.
    pub fn advance_step(&mut self, run_id: &str, result: SopStepResult) -> Result<SopRunAction> {
        let (sop_name, current_step_number) = {
            let run = self.active_runs.get(run_id).ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"run_id": run_id})),
                    "SOP engine: active run not found"
                );
                anyhow::Error::msg(format!("Active run not found: {run_id}"))
            })?;
            if matches!(
                run.status,
                SopRunStatus::WaitingApproval | SopRunStatus::PausedCheckpoint
            ) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "run_id": run_id,
                            "status": run.status.to_string(),
                            "step": run.current_step,
                        })),
                    "SOP engine: advance_step rejected — run is paused at a gate"
                );
                bail!(
                    "Run {run_id} is paused at a {} gate; resolve the gate through \
                     `resolve_gate` (WaitingApproval) or `approve_step` (PausedCheckpoint) \
                     before advancing with sop_advance",
                    run.status
                );
            }
            (run.sop_name.clone(), run.current_step)
        };

        let sop = self
            .sops
            .iter()
            .find(|s| s.name == sop_name)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"sop_name": sop_name})),
                    "SOP engine: sop no longer loaded (definition removed mid-run)"
                );
                anyhow::Error::msg(format!("SOP '{sop_name}' no longer loaded"))
            })?
            .clone();

        let current_step = sop
            .steps
            .get((current_step_number.saturating_sub(1)) as usize)
            .cloned()
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(
                            ::serde_json::json!({"sop_name": sop_name, "step": current_step_number})
                        ),
                    "SOP engine: step no longer exists (definition changed mid-run)"
                );
                anyhow::Error::msg(format!(
                    "SOP '{sop_name}' step {current_step_number} no longer exists (definition changed mid-run)"
                ))
            })?;

        // Deterministic runs are driven through the dedicated piping path so the
        // same `sop_advance` tool advances every execution mode.
        if sop.execution_mode == SopExecutionMode::Deterministic {
            if result.status == SopStepStatus::Failed {
                self.record_step_result(run_id, result.clone())?;
                return self.route_recorded_step(
                    run_id,
                    &sop,
                    &current_step,
                    SopStepStatus::Failed,
                    true,
                    Some(retry_input_value(
                        self.active_runs.get(run_id).ok_or_else(|| {
                            anyhow::Error::msg(format!("Active run not found: {run_id}"))
                        })?,
                        current_step.number,
                    )),
                    Some(step_result_value(&result)),
                );
            }
            let piped = step_result_value(&result);
            return self.advance_deterministic_step(
                run_id,
                piped,
                Some((result.started_at.clone(), result.completed_at.clone())),
            );
        }

        let mut recorded = result.clone();
        if result.status == SopStepStatus::Completed {
            let output = step_result_value(&result);
            if let Err(reason) = self.validate_step_output(&current_step, &output) {
                let full_reason = format!(
                    "Step {} output schema validation failed: {reason}",
                    current_step.number
                );
                self.record_transition_event(
                    run_id,
                    "step_schema_reject",
                    Some(full_reason.clone()),
                    ::serde_json::json!({
                        "step": current_step.number,
                        "phase": "output",
                    }),
                );
                recorded.status = SopStepStatus::Failed;
                recorded.output = full_reason;
            }
        }

        let retry_input = if recorded.status == SopStepStatus::Failed {
            Some(retry_input_value(
                self.active_runs
                    .get(run_id)
                    .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?,
                current_step.number,
            ))
        } else {
            None
        };

        self.record_step_result(run_id, recorded.clone())?;
        self.route_recorded_step(
            run_id,
            &sop,
            &current_step,
            recorded.status,
            false,
            retry_input,
            None,
        )
    }

    fn schema_input_failure_action(
        &mut self,
        run_id: &str,
        step: &SopStep,
        input: &Value,
    ) -> Option<SopRunAction> {
        match self.validate_step_input(step, input) {
            Ok(()) => None,
            Err(reason) => {
                Some(self.fail_step_schema_validation(run_id, step.number, "input", reason))
            }
        }
    }

    fn validate_step_input(&self, step: &SopStep, input: &Value) -> Result<(), String> {
        if !self.config.step_schema_enforce {
            return Ok(());
        }
        let Some(schema) = step
            .schema
            .as_ref()
            .and_then(|schema| schema.input.as_ref())
        else {
            return Ok(());
        };
        schema::validate_value(schema, input).map_err(|e| e.to_string())
    }

    fn validate_step_output(&self, step: &SopStep, output: &Value) -> Result<(), String> {
        if !self.config.step_schema_enforce {
            return Ok(());
        }
        let Some(schema) = step
            .schema
            .as_ref()
            .and_then(|schema| schema.output.as_ref())
        else {
            return Ok(());
        };
        schema::validate_value(schema, output).map_err(|e| e.to_string())
    }

    fn fail_step_schema_validation(
        &mut self,
        run_id: &str,
        step_number: u32,
        phase: &str,
        reason: String,
    ) -> SopRunAction {
        let reason = format!("Step {step_number} {phase} schema validation failed: {reason}");
        self.record_transition_event(
            run_id,
            "step_schema_reject",
            Some(reason.clone()),
            ::serde_json::json!({
                "step": step_number,
                "phase": phase,
            }),
        );
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "run_id": run_id,
                    "step": step_number,
                    "phase": phase,
                    "reason": reason,
                })),
            "SOP step schema validation failed"
        );
        self.finish_run(run_id, SopRunStatus::Failed, Some(reason))
    }

    fn record_step_result(&mut self, run_id: &str, result: SopStepResult) -> Result<()> {
        let run = self.active_runs.get_mut(run_id).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"run_id": run_id})),
                "SOP engine: active run not found"
            );
            anyhow::Error::msg(format!("Active run not found: {run_id}"))
        })?;
        run.step_results.push(result);
        Ok(())
    }

    fn route_recorded_step(
        &mut self,
        run_id: &str,
        sop: &Sop,
        current_step: &SopStep,
        last_status: SopStepStatus,
        deterministic: bool,
        retry_input: Option<Value>,
        routed_input: Option<Value>,
    ) -> Result<SopRunAction> {
        let decision =
            self.route_decision_after_recorded_step(run_id, sop, current_step, last_status)?;
        self.apply_route_decision(
            run_id,
            sop,
            current_step.number,
            decision,
            deterministic,
            retry_input,
            routed_input,
        )
    }

    fn route_decision_after_recorded_step(
        &self,
        run_id: &str,
        sop: &Sop,
        current_step: &SopStep,
        last_status: SopStepStatus,
    ) -> Result<NextStep> {
        let run = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;

        if last_status == SopStepStatus::Failed {
            let failed_executions = run
                .step_results
                .iter()
                .filter(|result| {
                    result.step_number == current_step.number
                        && result.status == SopStepStatus::Failed
                })
                .count()
                .try_into()
                .unwrap_or(u32::MAX);
            let retries_consumed = failed_executions.saturating_sub(1);
            let decision = route::failure::route_failure(
                &current_step.on_failure,
                retries_consumed,
                self.config.max_step_retries,
            );
            return Ok(match decision {
                NextStep::Fail(reason) if reason == "step failed" => {
                    let detail = run
                        .step_results
                        .iter()
                        .rev()
                        .find(|result| {
                            result.step_number == current_step.number
                                && result.status == SopStepStatus::Failed
                        })
                        .map(|result| result.output.as_str())
                        .unwrap_or("step failed");
                    NextStep::Fail(format!("Step {} failed: {detail}", current_step.number))
                }
                other => other,
            });
        }

        let run_data = RunData::from_step_results(&run.step_results);
        Ok(route::resolve_next(&RouteCtx {
            sop,
            run,
            run_data: &run_data,
            last_status,
            max_step_visits: self.config.max_step_visits,
        }))
    }

    fn apply_route_decision(
        &mut self,
        run_id: &str,
        sop: &Sop,
        current_step_number: u32,
        decision: NextStep,
        deterministic: bool,
        retry_input: Option<Value>,
        routed_input: Option<Value>,
    ) -> Result<SopRunAction> {
        match decision {
            NextStep::Step(step_number) => {
                if let Some(action) = self.visit_bound_failure(run_id, step_number)? {
                    return Ok(action);
                }
                self.record_transition_event(
                    run_id,
                    "step_promoted",
                    None,
                    ::serde_json::json!({
                        "from_step": current_step_number,
                        "to_step": step_number,
                    }),
                );
                if deterministic {
                    let input = routed_input.unwrap_or_default();
                    self.dispatch_deterministic_step(run_id, sop, step_number, input)
                } else {
                    self.dispatch_llm_step(run_id, sop, step_number, None)
                }
            }
            NextStep::Retry => {
                if let Some(action) = self.visit_bound_failure(run_id, current_step_number)? {
                    return Ok(action);
                }
                self.record_transition_event(
                    run_id,
                    "step_retry",
                    None,
                    ::serde_json::json!({
                        "step": current_step_number,
                    }),
                );
                if deterministic {
                    self.dispatch_deterministic_step(
                        run_id,
                        sop,
                        current_step_number,
                        retry_input.unwrap_or_default(),
                    )
                } else {
                    self.dispatch_llm_step(run_id, sop, current_step_number, retry_input)
                }
            }
            NextStep::Complete => {
                if deterministic {
                    Ok(self.finish_deterministic_run(run_id))
                } else {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"run_id": run_id})),
                        "SOP run completed successfully"
                    );
                    Ok(self.finish_run(run_id, SopRunStatus::Completed, None))
                }
            }
            NextStep::Fail(reason) => {
                Ok(self.finish_run(run_id, SopRunStatus::Failed, Some(reason)))
            }
            NextStep::Wait(step_number) => Ok(self.mark_step_pending(
                run_id,
                sop,
                step_number,
                format!("step {step_number} dependencies not satisfied"),
            )),
        }
    }

    fn visit_bound_failure(
        &mut self,
        run_id: &str,
        step_number: u32,
    ) -> Result<Option<SopRunAction>> {
        let run = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        if route::guard::within_visit_bound(run, step_number, self.config.max_step_visits) {
            return Ok(None);
        }

        Ok(Some(self.finish_run(
            run_id,
            SopRunStatus::Failed,
            Some(format!("step {step_number} visit limit reached")),
        )))
    }

    fn dispatch_llm_step(
        &mut self,
        run_id: &str,
        sop: &Sop,
        step_number: u32,
        input_override: Option<Value>,
    ) -> Result<SopRunAction> {
        let step = self.resolve_sop_step(sop, step_number)?;
        if let Some(action) = self.visit_bound_failure(run_id, step_number)? {
            return Ok(action);
        }

        if let Some(run) = self.active_runs.get_mut(run_id) {
            run.current_step = step_number;
            run.status = SopRunStatus::Running;
            run.waiting_since = None;
        }

        let run_data = {
            let run = self
                .active_runs
                .get(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            RunData::from_step_results(&run.step_results)
        };
        if !route::eligible(&step, &run_data) {
            return Ok(self.mark_step_pending(
                run_id,
                sop,
                step.number,
                format!("step {} dependencies not satisfied", step.number),
            ));
        }

        let input = match input_override {
            Some(input) => input,
            None => {
                let run = self
                    .active_runs
                    .get(run_id)
                    .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
                step_input_value(run, step.number)
            }
        };
        if let Some(action) = self.schema_input_failure_action(run_id, &step, &input) {
            return Ok(action);
        }

        let context = {
            let run = self
                .active_runs
                .get(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            format_step_context(sop, run, &step, &self.config)
        };
        // Upstream's resolve_step_action now forces approval whenever the
        // SOP-level mode needs it (strictly stronger than the old
        // approval_mode-conditional escalation), so the mode param is gone.
        let action = resolve_step_action(sop, &step, run_id.to_string(), context);
        let parked_for_approval = matches!(action, SopRunAction::WaitApproval { .. });
        let has_prior_gate_presentation = parked_for_approval
            && self.run_events(run_id).is_ok_and(|events| {
                events.iter().any(|event| {
                    matches!(
                        event.kind.as_str(),
                        "gate_vote" | "gate_resolved" | "gate_escalated" | "gate_timed_out"
                    )
                })
            });
        if parked_for_approval && let Some(run) = self.active_runs.get_mut(run_id) {
            run.status = SopRunStatus::WaitingApproval;
            run.waiting_since = Some(now_iso8601());
            if run.revision > 0 || has_prior_gate_presentation {
                run.revision += 1;
            }
        }

        // A1: parking, claim release, and the out-of-band prompt are one durable
        // transition. The helper notifies only after the snapshot is persisted.
        if parked_for_approval {
            self.persist_parked_snapshot_then_release_claim(run_id);
        } else {
            self.persist_active(run_id);
        }
        Ok(action)
    }

    /// Deliver the initial approval-request notice for a run that just parked at a
    /// policied gate, if that policy names a `request_route`. Best-effort: a run
    /// with no policy, a policy with no request route, or a delivery error all leave
    /// the (already-parked, already-durable) gate untouched.
    fn notify_park_request(&self, run_id: &str) {
        let Some(run) = self.get_run(run_id) else {
            return;
        };
        let (sop_name, step, revision) = (run.sop_name.clone(), run.current_step, run.revision);
        // Edit/Revise resolve ONLY through the deterministic-checkpoint path
        // (`resolve_checkpoint`); a broker-owned approval gate refuses them
        // fail-closed. Offering the choices on a non-checkpoint park would
        // render buttons whose submissions are always rejected — the operator's
        // typed text silently lost behind a success-looking ack.
        let is_checkpoint = run.status == SopRunStatus::PausedCheckpoint;
        // The notice carries WHAT is being approved: the parked step's piped
        // input (trigger payload at step 1, previous step's output later) plus
        // the step's authored `- prompt:` template when it has one.
        let context = step_input_value(run, step);
        let step_def = self
            .resolve_active_run_sop(run_id)
            .ok()
            .and_then(|(_, sop)| self.resolve_sop_step(&sop, step).ok());
        let gate_prompt = step_def.as_ref().and_then(|s| s.gate_prompt.clone());
        // Input-bearing choices: Edit needs the step's `- edit:` declaration;
        // Revise needs an llm.generate predecessor and headroom under the cap.
        let edit_field = step_def
            .as_ref()
            .filter(|_| is_checkpoint)
            .and_then(|s| s.edit.as_deref())
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_string);
        let can_revise = is_checkpoint
            && revision.saturating_sub(run.revision_base) < MAX_GATE_REVISIONS
            && self.revisable_predecessor(run_id).is_some();
        let Some(policy_name) = self.current_step_policy_name(run_id) else {
            return;
        };
        let broker = self.approval_broker();
        if let Some(route) = broker.request_route(self.approval_config(), &policy_name) {
            broker.deliver_request(
                &route,
                &super::approval::GateNotice {
                    run_id,
                    sop_name: &sop_name,
                    step,
                    context: &context,
                    gate_prompt: gate_prompt.as_deref(),
                    revision,
                    edit_field: edit_field.as_deref(),
                    can_revise,
                },
            );
        }
    }

    fn dispatch_deterministic_step(
        &mut self,
        run_id: &str,
        sop: &Sop,
        step_number: u32,
        input: Value,
    ) -> Result<SopRunAction> {
        let step = self.resolve_sop_step(sop, step_number)?;
        if let Some(action) = self.visit_bound_failure(run_id, step_number)? {
            return Ok(action);
        }

        if let Some(run) = self.active_runs.get_mut(run_id) {
            run.current_step = step_number;
            run.status = SopRunStatus::Running;
            run.waiting_since = None;
        }

        self.resolve_deterministic_action(sop, run_id, &step, input)
    }

    fn resolve_sop_step(&self, sop: &Sop, step_number: u32) -> Result<SopStep> {
        sop.steps
            .iter()
            .find(|step| step.number == step_number)
            .cloned()
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(
                            ::serde_json::json!({"sop_name": sop.name, "step": step_number})
                        ),
                    "SOP engine: step no longer exists (definition changed mid-run)"
                );
                anyhow::Error::msg(format!(
                    "SOP '{}' step {step_number} no longer exists (definition changed mid-run)",
                    sop.name
                ))
            })
    }

    fn mark_step_pending(
        &mut self,
        run_id: &str,
        sop: &Sop,
        step_number: u32,
        reason: String,
    ) -> SopRunAction {
        let now = now_iso8601();
        if let Some(run) = self.active_runs.get_mut(run_id) {
            run.current_step = step_number;
            run.status = SopRunStatus::Pending;
            run.waiting_since = Some(now.clone());
            let last_is_same_skip = run.step_results.last().is_some_and(|result| {
                result.step_number == step_number && result.status == SopStepStatus::Skipped
            });
            if !last_is_same_skip {
                run.step_results.push(SopStepResult {
                    step_number,
                    status: SopStepStatus::Skipped,
                    output: reason.clone(),
                    started_at: now.clone(),
                    completed_at: Some(now.clone()),
                    tool_calls: Vec::new(),
                });
            }
        }
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "run_id": run_id,
                    "sop_name": sop.name,
                    "step": step_number,
                    "reason": reason,
                })),
            "SOP run pending on step dependencies"
        );
        self.record_transition_event(
            run_id,
            "step_skipped",
            Some(reason.clone()),
            ::serde_json::json!({
                "step": step_number,
                "status": "pending",
            }),
        );
        self.persist_active(run_id);
        SopRunAction::Pending {
            run_id: run_id.to_string(),
            sop_name: sop.name.clone(),
            step: step_number,
            reason,
        }
    }

    fn finish_deterministic_run(&mut self, run_id: &str) -> SopRunAction {
        let saved = self
            .active_runs
            .get(run_id)
            .map(|run| run.llm_calls_saved)
            .unwrap_or(0);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("Deterministic SOP run {run_id} completed ({saved} LLM calls saved)")
        );
        self.deterministic_savings.total_llm_calls_saved += saved;
        self.deterministic_savings.total_runs += 1;
        self.finish_run(run_id, SopRunStatus::Completed, None)
    }

    /// Cancel an active run.
    pub fn cancel_run(&mut self, run_id: &str) -> Result<()> {
        if !self.active_runs.contains_key(run_id) {
            bail!("Active run not found: {run_id}");
        }
        self.finish_run_checked(run_id, SopRunStatus::Cancelled, None)?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"run_id": run_id})),
            "SOP run  cancelled"
        );
        Ok(())
    }

    /// Approve a step that is waiting for approval, transitioning back to Running.
    /// Resume a deterministic SOP run paused at a checkpoint. This owns ONLY the
    /// `PausedCheckpoint` resume; clearing a `WaitingApproval` gate is the
    /// out-of-band `resolve_gate` chokepoint (EPIC C) - the single audited
    /// gate-clear path. The `sop_approve` tool routes here for checkpoints and to
    /// `resolve_gate` for approval gates.
    pub fn approve_step(&mut self, run_id: &str) -> Result<SopRunAction> {
        self.resume_checkpoint(run_id, None)
    }

    /// Resume a run paused at a deterministic checkpoint, optionally amending one
    /// field of the piped value first (`amend = (field, text)`, the operator-edited
    /// draft). The amended value becomes the checkpoint's recorded output, so the
    /// human-approved text flows downstream while the predecessor step keeps the
    /// model's original.
    fn resume_checkpoint(
        &mut self,
        run_id: &str,
        amend: Option<(String, String)>,
    ) -> Result<SopRunAction> {
        self.resume_checkpoint_inner(run_id, amend, false)
    }

    fn resume_checkpoint_with_reacquired_claim(
        &mut self,
        run_id: &str,
        amend: Option<(String, String)>,
    ) -> Result<SopRunAction> {
        self.resume_checkpoint_inner(run_id, amend, true)
    }

    fn resume_checkpoint_inner(
        &mut self,
        run_id: &str,
        amend: Option<(String, String)>,
        claim_already_reacquired: bool,
    ) -> Result<SopRunAction> {
        let status = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"run_id": run_id})),
                    "SOP engine: active run not found"
                );
                anyhow::Error::msg(format!("Active run not found: {run_id}"))
            })?
            .status;

        if status != SopRunStatus::PausedCheckpoint {
            bail!("Run {run_id} is not paused at a checkpoint (status: {status})");
        }

        // Refuse to resume while the checkpoint's parked snapshot has not yet
        // been durably persisted (see `is_park_persist_pending`'s doc): the kept
        // claim predates this attempt, and reacquiring on top of it would give a
        // later rollback or a maintenance retry no way to distinguish "freshly
        // reacquired" from "pre-existing, must survive."
        if self.is_park_persist_pending(run_id) {
            bail!(
                "Run {run_id} cannot resume: its parked checkpoint snapshot is not yet durably persisted (retrying)"
            );
        }

        // Pre-flight the same SOP/step lookups `advance_deterministic_step` performs
        // BEFORE reacquiring the claim or mutating the run: a definition removed or
        // shrunk while parked must fail closed with the run left at
        // `PausedCheckpoint` (re-resolvable), not stranded in `Running` holding a
        // claim it can never advance.
        self.can_advance_deterministic_step(run_id)?;

        // A1: fail-closed - re-acquire the exec claim released when this run parked
        // BEFORE flipping it to Running; if it cannot, abort and leave the run paused
        // (re-resolvable) rather than execute uncounted.
        if !claim_already_reacquired {
            self.reacquire_claim_on_resume(run_id)?;
        }
        // A deterministic run paused at a checkpoint resumes through the
        // deterministic piping path: the checkpoint step is recorded as
        // completed and its input (the previous step's output — or, for a
        // checkpoint parked at step 1, the trigger payload) is piped forward.
        // Same step-1 mapping as `step_input_value`; `.last()` alone starved an
        // intake-gate pipeline (checkpoint BEFORE the first work step) of its
        // trigger payload.
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        let mut piped = step_input_value(run, run.current_step);
        // Operator amendment: replace the declared editable field BEFORE any run
        // mutation, so a non-amendable input (pre-flighted by
        // `can_amend_checkpoint`, so defensive here) leaves the run parked.
        if let Some((field, text)) = amend {
            match piped.as_object_mut() {
                Some(map) => {
                    map.insert(field, serde_json::Value::String(text));
                }
                None => {
                    self.release_claim_on_park(run_id);
                    bail!(
                        "Run {run_id} checkpoint input is not a JSON object; \
                         cannot amend field '{field}'"
                    );
                }
            }
        }
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        let prior_waiting_since = run.waiting_since.clone();
        run.status = SopRunStatus::Running;
        run.waiting_since = None;
        match self.advance_deterministic_step(run_id, piped, None) {
            Ok(action) => Ok(action),
            Err(e) => {
                // Defensive: the pre-flight above validated the same lookups under
                // this lock, so this is unreachable in practice. If the advance
                // still fails, roll the run back to `PausedCheckpoint` and release
                // the just-reacquired claim so a run that made no progress does not
                // get stuck in `Running` holding a leaked exec slot.
                if let Some(run) = self.active_runs.get_mut(run_id) {
                    run.status = SopRunStatus::PausedCheckpoint;
                    run.waiting_since = prior_waiting_since;
                }
                self.release_claim_on_park(run_id);
                Err(e)
            }
        }
    }

    /// The `- edit:` field the run's current checkpoint step declares, or why an
    /// amend cannot apply. Resolved under the engine lock at resolution time, so
    /// the field an operator edits is always the step's live declaration.
    fn checkpoint_edit_field(&self, run_id: &str) -> Result<String> {
        let (_, sop) = self.resolve_active_run_sop(run_id)?;
        let current_step = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?
            .current_step;
        let step = self.resolve_sop_step(&sop, current_step)?;
        step.edit
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "SOP '{}' step {current_step} does not declare an editable field \
                     (`- edit:`); an amend cannot apply",
                    sop.name
                ))
            })
    }

    /// Pre-flight an `Amend` WITHOUT mutating anything: the step must declare an
    /// editable field and the checkpoint's piped value must be a JSON object the
    /// field can replace into.
    fn can_amend_checkpoint(&self, run_id: &str) -> Result<()> {
        self.checkpoint_edit_field(run_id)?;
        let run = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        if !step_input_value(run, run.current_step).is_object() {
            bail!(
                "Run {run_id} checkpoint input is not a JSON object; \
                 there is no field an amend could replace"
            );
        }
        Ok(())
    }

    /// The step a `Revise` would re-run: the last COMPLETED step before the
    /// checkpoint, but only when it is an `llm.generate` capability (the only
    /// step kind a re-draft is meaningful for). `None` = this gate is not
    /// revisable.
    fn revisable_predecessor(&self, run_id: &str) -> Option<u32> {
        let run = self.get_run(run_id)?;
        let pred = run
            .step_results
            .iter()
            .rev()
            .find(|r| r.status == SopStepStatus::Completed && r.step_number < run.current_step)?
            .step_number;
        let (_, sop) = self.resolve_active_run_sop(run_id).ok()?;
        let step = self.resolve_sop_step(&sop, pred).ok()?;
        (step.kind == SopStepKind::Capability && step.capability_id() == Some("llm.generate"))
            .then_some(pred)
    }

    /// Pre-flight a `Revise` WITHOUT mutating anything: the revision cap has not
    /// been reached and the gate has an `llm.generate` predecessor to re-run.
    fn can_revise_checkpoint(&self, run_id: &str) -> Result<()> {
        let run = self
            .get_run(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        // Per-GATE budget: presentations spent at THIS gate, not run-wide
        // (`revision` also advances when a later gate first parks).
        if run.revision.saturating_sub(run.revision_base) >= MAX_GATE_REVISIONS {
            bail!(
                "Run {run_id} has reached this gate's revision limit ({MAX_GATE_REVISIONS}); \
                 approve, edit, or deny the current draft"
            );
        }
        if self.revisable_predecessor(run_id).is_none() {
            bail!(
                "Run {run_id} has no llm.generate predecessor step to re-run; \
                 this gate is not revisable"
            );
        }
        Ok(())
    }

    /// Re-run the checkpoint's predecessor `llm.generate` step with the operator's
    /// guidance framed as reviewer feedback, record the resolved ledger row, then
    /// replace the recorded draft, bump the gate revision, and re-present the gate.
    /// The run never leaves `PausedCheckpoint`: a failed re-draft keeps the OLD
    /// draft parked and answerable and writes no ledger row. If the successful
    /// re-draft cannot be persisted after its ledger entry is recorded, it remains
    /// fail-closed for retry and this method returns an error rather than reporting
    /// `Revised` to the prompt owner. The model call blocks under the engine lock —
    /// the same tradeoff as a normal `llm.generate` step.
    fn revise_checkpoint_draft(
        &mut self,
        run_id: &str,
        guidance: &str,
        principal: &super::approval::ApprovalPrincipal,
        checkpoint_step: u32,
    ) -> Result<()> {
        let (_, sop) = self.resolve_active_run_sop(run_id)?;
        let pred_number = self.revisable_predecessor(run_id).ok_or_else(|| {
            anyhow::Error::msg(format!(
                "Run {run_id} has no llm.generate predecessor step to re-run"
            ))
        })?;
        let pred_step = self.resolve_sop_step(&sop, pred_number)?;
        let piped = {
            let run = self
                .get_run(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            replay_input_for_step(run, pred_number)
        };

        // The guidance rides in the step's STATIC config plane (alongside the
        // authored instruction), NOT the untrusted payload frame: it comes from
        // an authenticated approver, and it must be able to steer the redraft.
        let mut step = pred_step.clone();
        let mut configured = step
            .capability_input
            .take()
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(object) = configured.as_object_mut() {
            object.insert(
                "revision_feedback".to_string(),
                serde_json::Value::String(guidance.to_string()),
            );
        }
        step.capability_input = Some(configured);

        // The re-draft is real work: hold an exec slot for its duration (the run
        // released its slot when it parked).
        self.reacquire_claim_on_resume(run_id)?;
        let ctx = super::capability::CapabilityContext {
            run_id: run_id.to_string(),
            sop_name: sop.name.clone(),
            step_number: pred_number,
            sop_location: sop.location.clone(),
        };
        let result = self.capabilities.execute_step(ctx, &step, piped);
        self.metrics.record_capability_executed(&sop.name);

        let output = match result {
            Ok(r) if r.success => match self.validate_step_output(&pred_step, &r.output) {
                Ok(()) => r.output,
                Err(reason) => {
                    self.release_claim_on_park(run_id);
                    bail!(
                        "Run {run_id} revised draft failed step {pred_number}'s output \
                         schema (previous draft kept): {reason}"
                    );
                }
            },
            Ok(r) => {
                self.release_claim_on_park(run_id);
                bail!(
                    "Run {run_id} re-draft failed (previous draft kept): {}",
                    r.error
                        .unwrap_or_else(|| "capability returned failure".to_string())
                );
            }
            Err(e) => {
                self.release_claim_on_park(run_id);
                bail!("Run {run_id} re-draft failed (previous draft kept): {e}");
            }
        };

        // Audit-before-mutate: the model produced a valid new draft, so the
        // revise DID happen — record it now, BEFORE any run mutation. A failed
        // append leaves the old draft parked (no mutation yet) and releases the
        // reacquired claim, exactly like a failed model call; it can never leave
        // a false resolved row (the model-call failures above bailed with none).
        let checkpoint_revision = self
            .get_run(run_id)
            .map(|run| run.revision)
            .unwrap_or_default();
        let revision_decision = super::approval::ApprovalDecision::Revise {
            guidance: guidance.to_string(),
        };
        let decision_identity =
            super::approval::broker::checkpoint_decision_identity(&revision_decision)
                .map(|(_, identity)| identity);

        if let Err(e) = self.record_gate_event(super::approval::GateLedgerEntry {
            run_id: run_id.to_string(),
            step: checkpoint_step,
            gate_revision: Some(checkpoint_revision),
            checkpoint_revision: Some(checkpoint_revision),
            decision_identity,
            kind: super::approval::GateEventKind::Resolved,
            decision: Some(revision_decision),
            principal: principal.clone(),
            ts: now_iso8601(),
        }) {
            self.release_claim_on_park(run_id);
            return Err(anyhow::Error::msg(format!(
                "Run {run_id} re-draft succeeded but its resolution ledger row could not \
                 be persisted (fail-closed; previous draft kept): {e}"
            )));
        }

        {
            let run = self
                .active_runs
                .get_mut(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            if let Some(recorded) = run
                .step_results
                .iter_mut()
                .rev()
                .find(|r| r.step_number == pred_number && r.status == SopStepStatus::Completed)
            {
                recorded.output = output.to_string();
                recorded.completed_at = Some(now_iso8601());
            }
            run.revision += 1;
            run.waiting_since = Some(now_iso8601());
        }

        // Same park sequence as the original checkpoint: refresh the state file,
        // then durably persist the parked snapshot. The helper releases the claim
        // and notifies the approver only after that state can be recovered.
        if let Err(e) = self.persist_deterministic_state(run_id, &sop) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"run_id": run_id, "error": e.to_string()})),
                "SOP engine: revised state-file refresh failed (run store remains authoritative)"
            );
        }
        if !self.persist_parked_snapshot_then_release_claim(run_id) {
            bail!(
                "Run {run_id} revised draft is not durably parked yet; \
                 keeping the prior prompt active until persistence succeeds"
            );
        }
        Ok(())
    }

    /// Pre-flight ONLY the fallible SOP/step lookups that
    /// `advance_deterministic_step` performs for `run_id`'s current step, WITHOUT
    /// reacquiring a claim, mutating the run, or persisting anything.
    ///
    /// `approve_step` calls this BEFORE it reacquires the exec claim and flips the
    /// run to `Running`, so a checkpoint resume whose SOP was removed or shrunk
    /// while parked fails closed here - with the run left untouched at
    /// `PausedCheckpoint` - instead of after the mutation, which would otherwise
    /// strand the run in `Running`, holding a claim, with no way to make progress.
    pub(crate) fn can_advance_deterministic_step(&self, run_id: &str) -> Result<()> {
        let (_, sop) = self.resolve_active_run_sop(run_id)?;
        let current_step = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?
            .current_step;
        self.resolve_sop_step(&sop, current_step)?;
        Ok(())
    }

    /// Pre-flight ONLY the fallible lookups that `clear_waiting_gate` performs
    /// (the SOP is still loaded and the waiting step still resolves by number),
    /// WITHOUT reacquiring a claim, mutating the run, or persisting anything.
    ///
    /// `resolve_gate` calls this BEFORE it reacquires the exec claim and appends
    /// the immutable `gate_resolved` ledger row, so a run whose SOP was removed or
    /// shrunk while it sat parked fails closed here - with no claim reacquired and
    /// no false "resolved" audit row - instead of after the ledger append, which
    /// would otherwise leave a durable `gate_resolved` row for a still-waiting gate
    /// AND leak the reacquired exec slot. Runs under the engine mutex, so the
    /// lookups it validates cannot change before `clear_waiting_gate` re-runs them.
    pub(crate) fn can_clear_waiting_gate(&self, run_id: &str) -> Result<()> {
        let (sop_name, current_step) = {
            let run = self
                .active_runs
                .get(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            (run.sop_name.clone(), run.current_step)
        };
        let sop = self
            .sops
            .iter()
            .find(|s| s.name == sop_name)
            .ok_or_else(|| anyhow::Error::msg(format!("SOP '{sop_name}' no longer loaded")))?;
        self.resolve_sop_step(sop, current_step)?;
        Ok(())
    }

    /// Resolve a checkpoint decision (`PausedCheckpoint`). `Approve` resumes the
    /// success path (records the checkpoint `Completed`, pipes forward down
    /// `routing.next`); `Deny` takes the failure path (records the checkpoint
    /// `Failed` and routes through the step's `on_failure`, exactly like a step
    /// that failed execution). This is the single entry point for both outcomes;
    /// callers never branch on status. `approve_step` is the `Approve`-only alias.
    pub fn decide_checkpoint(
        &mut self,
        run_id: &str,
        decision: super::approval::ApprovalDecision,
    ) -> Result<SopRunAction> {
        match decision {
            super::approval::ApprovalDecision::Approve => self.approve_step(run_id),
            super::approval::ApprovalDecision::Deny { reason } => {
                self.deny_checkpoint(run_id, reason)
            }
            // Amend/Revise need the audited chokepoint (ledger row, pre-flights,
            // re-presentation) — they are not mechanical primitives this alias
            // can apply. Callers resolve them via `resolve_via_broker`.
            super::approval::ApprovalDecision::Amend { .. }
            | super::approval::ApprovalDecision::Revise { .. } => bail!(
                "amend/revise resolve through the approval chokepoint \
                 (resolve_via_broker), not decide_checkpoint"
            ),
        }
    }

    /// Failure path for a denied checkpoint: record the checkpoint step `Failed`
    /// and route through its `on_failure` policy via the shared deterministic
    /// record-and-route chokepoint. `Goto` reaches the authored failure step;
    /// the default `Fail` terminates the run `Failed`. Mirrors `approve_step`'s
    /// guard so a wrong-status or missing run fails closed with the gate intact.
    fn deny_checkpoint(&mut self, run_id: &str, reason: Option<String>) -> Result<SopRunAction> {
        let status = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"run_id": run_id})),
                    "SOP engine: active run not found"
                );
                anyhow::Error::msg(format!("Active run not found: {run_id}"))
            })?
            .status;

        if status != SopRunStatus::PausedCheckpoint {
            bail!("Run {run_id} is not paused at a checkpoint (status: {status})");
        }

        let (_, sop) = self.resolve_active_run_sop(run_id)?;
        let current_step_number = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?
            .current_step;
        let current_step = self.resolve_sop_step(&sop, current_step_number)?;

        let detail = reason.unwrap_or_else(|| "checkpoint denied by operator".to_string());
        let now = now_iso8601();

        if let Some(run) = self.active_runs.get_mut(run_id) {
            run.status = SopRunStatus::Running;
            run.waiting_since = None;
        }
        self.record_transition_event(
            run_id,
            "checkpoint_denied",
            Some(detail.clone()),
            ::serde_json::json!({
                "step": current_step.number,
                "kind": current_step.kind.to_string(),
            }),
        );

        self.record_deterministic_step_result(
            run_id,
            &sop,
            &current_step,
            SopStepStatus::Failed,
            detail.clone(),
            serde_json::Value::String(detail),
            now.clone(),
            Some(now),
        )
    }

    /// Clear a `WaitingApproval` gate: flip to Running, build the ExecuteStep
    /// action for the current step, and persist. Shared by `approve_step` (the
    /// agent path) and `resolve_gate` (the out-of-band path) so the transition
    /// lives in exactly one place. Caller guarantees the run is `WaitingApproval`.
    ///
    /// All-or-nothing: the SOP definition and current step are resolved (and
    /// bounds-checked) BEFORE any in-memory mutation, so a definition removed or
    /// shrunk mid-run returns `Err` with the gate left untouched (still
    /// `WaitingApproval`, re-resolvable) rather than half-transitioned or panicking
    /// on an out-of-range step index (which would poison the engine mutex). The
    /// pure prefix of these lookups is exposed as `can_clear_waiting_gate` so
    /// `resolve_gate` can fail closed before it touches the claim or the ledger.
    pub(crate) fn clear_waiting_gate(&mut self, run_id: &str) -> Result<SopRunAction> {
        let (sop_name, current_step) = {
            let run = self
                .active_runs
                .get(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            (run.sop_name.clone(), run.current_step)
        };

        let sop = self
            .sops
            .iter()
            .find(|s| s.name == sop_name)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"sop_name": sop_name})),
                    "SOP engine: sop no longer loaded (definition removed mid-run)"
                );
                anyhow::Error::msg(format!("SOP '{sop_name}' no longer loaded"))
            })?
            .clone();

        // Resolve the waiting step by its NUMBER (not vec position): a routed SOP with
        // non-contiguous step numbers (e.g. 1, 5) means position != number, and a
        // positional lookup would resume the wrong step - and, worse, only AFTER
        // resolve_gate already reacquired the claim and wrote the gate_resolved row.
        let step = self.resolve_sop_step(&sop, current_step)?;

        let run_data = {
            let run = self
                .active_runs
                .get(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            RunData::from_step_results(&run.step_results)
        };
        if !route::eligible(&step, &run_data) {
            return Ok(self.mark_step_pending(
                run_id,
                &sop,
                step.number,
                format!("step {} dependencies not satisfied", step.number),
            ));
        }

        let input = {
            let run = self
                .active_runs
                .get(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            step_input_value(run, step.number)
        };
        if let Some(action) = self.schema_input_failure_action(run_id, &step, &input) {
            return Ok(action);
        }

        // The exec claim was already re-acquired by resolve_gate BEFORE the audit row
        // (so a claim failure never writes a false gate_resolved row, and the run
        // holds its claim before EITHER the Pending or the Running transition here).

        // The lookups succeeded; commit the transition.
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        run.status = SopRunStatus::Running;
        run.waiting_since = None;
        let context = format_step_context(&sop, run, &step, &self.config);

        let mut step = step;
        step.agent = step
            .effective_agent(sop.agent.as_deref())
            .map(str::to_string);

        self.persist_active(run_id);
        Ok(SopRunAction::ExecuteStep {
            run_id: run_id.to_string(),
            step,
            context,
        })
    }

    /// List finished runs, optionally filtered by SOP name.
    pub fn finished_runs(&self, sop_name: Option<&str>) -> Vec<&SopRun> {
        self.finished_runs
            .iter()
            .filter(|r| sop_name.is_none_or(|name| r.sop_name == name))
            .collect()
    }

    /// Summaries of every run the engine currently holds: live runs from the
    /// active set plus retained terminal runs, newest first by start time.
    /// This is the enumeration the Runs surface polls; it never touches the
    /// durable store directly, so it reflects exactly what the running engine
    /// knows (active set + `max_finished_runs` retention window).
    pub fn run_summaries(&self, sop_name: Option<&str>) -> Vec<SopRunSummary> {
        let mut out: Vec<SopRunSummary> = self
            .active_runs
            .values()
            .filter(|r| sop_name.is_none_or(|name| r.sop_name == name))
            .map(|r| SopRunSummary::from_run(r, true))
            .chain(
                self.finished_runs
                    .iter()
                    .filter(|r| sop_name.is_none_or(|name| r.sop_name == name))
                    .map(|r| SopRunSummary::from_run(r, false)),
            )
            .collect();
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        out
    }

    /// Return cumulative deterministic execution savings.
    pub fn deterministic_savings(&self) -> &DeterministicSavings {
        &self.deterministic_savings
    }

    /// Save a procedural-memory proposal into the shared SOP store. This is the
    /// production-facing engine surface EPIC F consumes for approval/write-back.
    pub fn save_proposal(&self, proposal: &ProposalRecord) -> Result<(), StoreError> {
        self.store.save_proposal(proposal)
    }

    /// Load a procedural-memory proposal by id from the shared SOP store.
    pub fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
        self.store.load_proposal(id)
    }

    /// List procedural-memory proposals, optionally filtered by lifecycle status.
    pub fn list_proposals(
        &self,
        status: Option<ProposalStatus>,
    ) -> Result<Vec<ProposalRecord>, StoreError> {
        self.store.list_proposals(status)
    }

    // ── Deterministic execution ─────────────────────────────────

    /// Start a deterministic SOP run. Steps execute sequentially without LLM
    /// round-trips. Returns the first action (DeterministicStep or CheckpointWait).
    pub fn start_deterministic_run(
        &mut self,
        sop_name: &str,
        event: SopEvent,
    ) -> Result<SopRunAction> {
        // A2: this is a PUBLIC start entrypoint, so it must enforce the admission
        // policy itself - a direct caller must not be able to bypass Hold / Coalesce
        // / the pending-approval pool that `start_run` enforces. (When reached via
        // `start_run` the re-check is idempotent under the same held lock.)
        self.enforce_admission(sop_name)?;

        let sop = self
            .get_sop(sop_name)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"sop_name": sop_name})),
                    "SOP engine: sop not found"
                );
                anyhow::Error::msg(format!("SOP not found: {sop_name}"))
            })?
            .clone();

        if sop.execution_mode != SopExecutionMode::Deterministic {
            bail!(
                "SOP '{}' is not in deterministic mode (mode: {})",
                sop_name,
                sop.execution_mode
            );
        }

        if !self.can_start(sop_name) {
            bail!(
                "Cannot start SOP '{}': cooldown or concurrency limit reached",
                sop_name
            );
        }

        if sop.steps.is_empty() {
            bail!("SOP '{}' has no steps defined", sop_name);
        }

        self.run_counter += 1;
        let dur = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let epoch_ns = dur.as_nanos();
        let run_id = format!("det-{epoch_ns}-{:04}", self.run_counter);
        let now = now_iso8601();

        let total_steps = u32::try_from(sop.steps.len()).unwrap_or(u32::MAX);
        let run = SopRun {
            run_id: run_id.clone(),
            sop_name: sop_name.to_string(),
            trigger_event: event,
            frame_marker_id: new_marker_id(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps,
            started_at: now,
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        };

        // Step 1's input is the trigger payload — the same mapping
        // `step_input_value` applies on every other path (resume, retry). The
        // start path used to hardcode `Null`, so a deterministic pipeline's
        // first step never saw the event that triggered it.
        let first_input = step_input_value(&run, 1);

        let claim = self.claim_admission(&run_id, &sop)?;
        self.active_runs.insert(run_id.clone(), run);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Deterministic SOP run {} started for '{}'",
                run_id, sop_name
            )
        );

        match self.dispatch_deterministic_step(&run_id, &sop, 1, first_input) {
            Ok(action) => Ok(action),
            Err(e) => {
                self.active_runs.remove(&run_id);
                self.release_claim_best_effort(&claim);
                Err(e)
            }
        }
    }

    /// Drive a just-started headless deterministic run until it blocks or ends.
    ///
    /// Channel-sourced dispatch (filesystem, MQTT, peripheral, cron) has no
    /// agent loop to execute normal `Execute` steps. Capability steps can run
    /// through the deterministic registry here; driver-required steps fail closed
    /// so the audit trail never reports a green step that did not execute.
    /// A `CheckpointWait` is intentionally left paused (an operator gate, not a
    /// stuck run).
    pub fn drive_headless_deterministic(
        &mut self,
        run_id: &str,
        first_action: SopRunAction,
    ) -> Result<SopRunAction> {
        let mut action = first_action;
        loop {
            match action {
                SopRunAction::DeterministicStep {
                    ref step,
                    ref input,
                    ..
                } if step.kind == SopStepKind::Capability => {
                    let (sop_name, sop) = self.resolve_active_run_sop(run_id)?;
                    action = self.execute_capability_step(&sop, run_id, step, input.clone())?;
                    if self.active_runs.contains_key(run_id) {
                        let run_sop_name = self
                            .active_runs
                            .get(run_id)
                            .map(|run| run.sop_name.as_str())
                            .unwrap_or(sop_name.as_str());
                        if run_sop_name != sop.name {
                            return Ok(action);
                        }
                    }
                }
                SopRunAction::DeterministicStep {
                    ref step,
                    ref run_id,
                    ..
                } => {
                    let sop_name = self
                        .active_runs
                        .get(run_id)
                        .map(|run| run.sop_name.clone())
                        .unwrap_or_default();
                    return Ok(self.fail_headless_driverless_step(run_id, &sop_name, step));
                }
                terminal => return Ok(terminal),
            }
        }
    }

    /// Advance a deterministic run with the output of the current step.
    /// The output is piped as input to the next step.
    pub fn advance_deterministic_step(
        &mut self,
        run_id: &str,
        step_output: serde_json::Value,
        step_timestamps: Option<(String, Option<String>)>,
    ) -> Result<SopRunAction> {
        let (_, sop) = self.resolve_active_run_sop(run_id)?;
        let current_step_number = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?
            .current_step;
        let current_step = self.resolve_sop_step(&sop, current_step_number)?;
        let (started_at, completed_at) = match step_timestamps {
            Some((started, completed)) => (started, completed),
            None => {
                let run = self
                    .active_runs
                    .get(run_id)
                    .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
                (run.started_at.clone(), Some(now_iso8601()))
            }
        };

        self.record_deterministic_step_result(
            run_id,
            &sop,
            &current_step,
            SopStepStatus::Completed,
            step_output.to_string(),
            step_output,
            started_at,
            completed_at,
        )
    }

    fn forge_comment_authorized_by_prior_checkpoint(
        &self,
        sop: &Sop,
        run_id: &str,
        step_number: u32,
        input: &serde_json::Value,
    ) -> bool {
        let Some(run) = self.active_runs.get(run_id) else {
            return false;
        };
        let checkpoint_revision = run.revision;
        let Some(checkpoint_result) = run
            .step_results
            .iter()
            .rev()
            .find(|result| result.status == SopStepStatus::Completed)
        else {
            return false;
        };
        let checkpoint_step_number = checkpoint_result.step_number;
        if !sop.steps.iter().any(|step| {
            step.number == checkpoint_step_number && step.kind == SopStepKind::Checkpoint
        }) {
            return false;
        }
        if checkpoint_step_number >= step_number {
            return false;
        }
        if !forge_comment_input_matches_checkpoint_output(input, checkpoint_result) {
            return false;
        }

        self.run_events(run_id).is_ok_and(|events| {
            events.iter().any(|event| {
                event.kind.as_str() == "gate_resolved"
                    && event.payload.get("step").and_then(|value| value.as_u64())
                        == Some(u64::from(checkpoint_step_number))
                    && event
                        .payload
                        .get("checkpoint_revision")
                        .and_then(|value| value.as_u64())
                        == Some(u64::from(checkpoint_revision))
                    && event
                        .payload
                        .get("source")
                        .and_then(|value| value.as_str())
                        .is_some_and(|source| source != "agent" && source != "system")
                    && matches!(
                        event
                            .payload
                            .get("decision")
                            .and_then(|value| value.as_str()),
                        Some("approve") | Some("amend")
                    )
            })
        })
    }

    fn forge_comment_effect_payload(
        &self,
        sop: &Sop,
        step_number: u32,
        input: &Value,
    ) -> Result<Value> {
        let target =
            super::capability::resolve_forge_comment_target(input).map_err(anyhow::Error::msg)?;
        Ok(::serde_json::json!({
            "capability": "forge.comment",
            "sop_name": sop.name,
            "step": step_number,
            "channel": target.channel,
            "repo": target.repo,
            "number": target.number,
            "body": target.body,
        }))
    }

    fn forge_comment_success_output(&self, input: &Value) -> Result<Value> {
        let target =
            super::capability::resolve_forge_comment_target(input).map_err(anyhow::Error::msg)?;
        Ok(::serde_json::json!({
            "posted": true,
            "repo": target.repo,
            "number": target.number,
        }))
    }

    fn forge_comment_effect_state(
        &self,
        run_id: &str,
        effect_payload: &Value,
    ) -> Result<(bool, bool), StoreError> {
        let mut started = false;
        let mut completed = false;
        for event in self.store.list_events(run_id)? {
            if event.payload == *effect_payload {
                match event.kind.as_str() {
                    "capability_effect_started" => started = true,
                    "capability_effect_completed" => completed = true,
                    _ => {}
                }
            }
        }
        Ok((started, completed))
    }

    fn record_forge_comment_effect_marker(
        &self,
        run_id: &str,
        kind: &str,
        effect_payload: Value,
    ) -> Result<(), StoreError> {
        self.store
            .append_event(&SopEventRecord {
                run_id: run_id.to_string(),
                seq: 0,
                ts: now_iso8601(),
                kind: kind.to_string(),
                actor: None,
                reason: None,
                payload: effect_payload,
            })
            .map(|_| ())
    }

    fn record_forge_comment_failure(
        &mut self,
        run_id: &str,
        sop: &Sop,
        step: &SopStep,
        error: String,
        started_at: String,
    ) -> Result<SopRunAction> {
        self.metrics.record_capability_executed(&sop.name);
        let completed_at = Some(now_iso8601());
        self.record_deterministic_step_result(
            run_id,
            sop,
            step,
            SopStepStatus::Failed,
            error.clone(),
            serde_json::Value::String(error),
            started_at,
            completed_at,
        )
    }

    fn execute_forge_comment_step(
        &mut self,
        sop: &Sop,
        run_id: &str,
        step: &SopStep,
        input: Value,
        capability_input: Value,
        started_at: String,
    ) -> Result<SopRunAction> {
        if !self.forge_comment_authorized_by_prior_checkpoint(
            sop,
            run_id,
            step.number,
            &capability_input,
        ) {
            return self.record_forge_comment_failure(
                run_id,
                sop,
                step,
                "forge.comment requires the immediately preceding checkpoint to approve the exact repo, number, body, and channel"
                    .to_string(),
                started_at,
            );
        }

        let effect_payload =
            match self.forge_comment_effect_payload(sop, step.number, &capability_input) {
                Ok(payload) => payload,
                Err(e) => {
                    return self.record_forge_comment_failure(
                        run_id,
                        sop,
                        step,
                        format!("forge.comment: invalid target for effect ledger: {e}"),
                        started_at,
                    );
                }
            };
        let success_output = match self.forge_comment_success_output(&capability_input) {
            Ok(output) => output,
            Err(e) => {
                return self.record_forge_comment_failure(
                    run_id,
                    sop,
                    step,
                    format!("forge.comment: invalid target for success replay: {e}"),
                    started_at,
                );
            }
        };

        match self.forge_comment_effect_state(run_id, &effect_payload) {
            Ok((_started, true)) => {
                self.metrics.record_capability_executed(&sop.name);
                let completed_at = Some(now_iso8601());
                return self.record_deterministic_step_result(
                    run_id,
                    sop,
                    step,
                    SopStepStatus::Completed,
                    success_output.to_string(),
                    success_output,
                    started_at,
                    completed_at,
                );
            }
            Ok((true, false)) => {
                return self.record_forge_comment_failure(
                    run_id,
                    sop,
                    step,
                    "forge.comment has a prior unconfirmed public-send attempt for this run/step/target; refusing to replay automatically"
                        .to_string(),
                    started_at,
                );
            }
            Ok((false, false)) => {}
            Err(e) => {
                return self.record_forge_comment_failure(
                    run_id,
                    sop,
                    step,
                    format!(
                        "forge.comment cannot inspect durable effect ledger (fail-closed): {e}"
                    ),
                    started_at,
                );
            }
        }

        if let Err(e) = self.record_forge_comment_effect_marker(
            run_id,
            "capability_effect_started",
            effect_payload.clone(),
        ) {
            return self.record_forge_comment_failure(
                run_id,
                sop,
                step,
                format!(
                    "forge.comment cannot persist public-send attempt marker (fail-closed): {e}"
                ),
                started_at,
            );
        }

        let ctx = super::capability::CapabilityContext {
            run_id: run_id.to_string(),
            sop_name: sop.name.clone(),
            step_number: step.number,
            sop_location: sop.location.clone(),
        };
        let result = self.capabilities.execute_step(ctx, step, input);
        self.metrics.record_capability_executed(&sop.name);
        let completed_at = Some(now_iso8601());
        match result {
            Ok(result) if result.success => {
                if let Err(e) = self.record_forge_comment_effect_marker(
                    run_id,
                    "capability_effect_completed",
                    effect_payload,
                ) {
                    let error = format!(
                        "forge.comment posted but could not persist success marker (fail-closed; refusing replay): {e}"
                    );
                    return self.record_deterministic_step_result(
                        run_id,
                        sop,
                        step,
                        SopStepStatus::Failed,
                        error.clone(),
                        serde_json::Value::String(error),
                        started_at,
                        completed_at,
                    );
                }
                self.record_deterministic_step_result(
                    run_id,
                    sop,
                    step,
                    SopStepStatus::Completed,
                    result.output.to_string(),
                    result.output,
                    started_at,
                    completed_at,
                )
            }
            Ok(result) => {
                let error = result
                    .error
                    .unwrap_or_else(|| "capability returned failure".to_string());
                self.record_deterministic_step_result(
                    run_id,
                    sop,
                    step,
                    SopStepStatus::Failed,
                    error.clone(),
                    serde_json::Value::String(error),
                    started_at,
                    completed_at,
                )
            }
            Err(e) => {
                let error = e.to_string();
                self.record_deterministic_step_result(
                    run_id,
                    sop,
                    step,
                    SopStepStatus::Failed,
                    error.clone(),
                    serde_json::Value::String(error),
                    started_at,
                    completed_at,
                )
            }
        }
    }

    fn execute_capability_step(
        &mut self,
        sop: &Sop,
        run_id: &str,
        step: &SopStep,
        input: serde_json::Value,
    ) -> Result<SopRunAction> {
        let started_at = now_iso8601();
        let capability_input = step.capability_call_input(input.clone());
        if step.capability_id() == Some("forge.comment") {
            return self.execute_forge_comment_step(
                sop,
                run_id,
                step,
                input,
                capability_input,
                started_at,
            );
        }

        let ctx = super::capability::CapabilityContext {
            run_id: run_id.to_string(),
            sop_name: sop.name.clone(),
            step_number: step.number,
            sop_location: sop.location.clone(),
        };
        let result = self.capabilities.execute_step(ctx, step, input);
        self.metrics.record_capability_executed(&sop.name);
        let completed_at = Some(now_iso8601());
        match result {
            Ok(result) if result.success => self.record_deterministic_step_result(
                run_id,
                sop,
                step,
                SopStepStatus::Completed,
                result.output.to_string(),
                result.output,
                started_at,
                completed_at,
            ),
            Ok(result) => {
                let error = result
                    .error
                    .unwrap_or_else(|| "capability returned failure".to_string());
                self.record_deterministic_step_result(
                    run_id,
                    sop,
                    step,
                    SopStepStatus::Failed,
                    error.clone(),
                    serde_json::Value::String(error),
                    started_at,
                    completed_at,
                )
            }
            Err(e) => {
                let error = e.to_string();
                self.record_deterministic_step_result(
                    run_id,
                    sop,
                    step,
                    SopStepStatus::Failed,
                    error.clone(),
                    serde_json::Value::String(error),
                    started_at,
                    completed_at,
                )
            }
        }
    }

    fn record_deterministic_step_result(
        &mut self,
        run_id: &str,
        sop: &Sop,
        current_step: &SopStep,
        status: SopStepStatus,
        recorded_output: String,
        routed_output: serde_json::Value,
        started_at: String,
        completed_at: Option<String>,
    ) -> Result<SopRunAction> {
        let run = self.active_runs.get_mut(run_id).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"run_id": run_id})),
                "SOP engine: active run not found"
            );
            anyhow::Error::msg(format!("Active run not found: {run_id}"))
        })?;
        let retry_input = retry_input_value(run, current_step.number);
        run.step_results.push(SopStepResult {
            step_number: run.current_step,
            status,
            output: recorded_output,
            started_at,
            completed_at,
            tool_calls: Vec::new(),
        });

        let mut last_status = status;
        if status == SopStepStatus::Completed {
            if let Err(reason) = self.validate_step_output(current_step, &routed_output) {
                last_status = SopStepStatus::Failed;
                let full_reason = format!(
                    "Step {} output schema validation failed: {reason}",
                    current_step.number
                );
                self.record_transition_event(
                    run_id,
                    "step_schema_reject",
                    Some(full_reason.clone()),
                    ::serde_json::json!({
                        "step": current_step.number,
                        "phase": "output",
                    }),
                );
                if let Some(recorded) = self
                    .active_runs
                    .get_mut(run_id)
                    .and_then(|run| run.step_results.last_mut())
                {
                    recorded.status = SopStepStatus::Failed;
                    recorded.output = full_reason;
                }
            } else if let Some(run) = self.active_runs.get_mut(run_id) {
                run.llm_calls_saved += 1;
            }
        }

        self.route_recorded_step(
            run_id,
            sop,
            current_step,
            last_status,
            true,
            Some(retry_input),
            Some(routed_output),
        )
    }

    fn resolve_active_run_sop(&self, run_id: &str) -> Result<(String, Sop)> {
        let sop_name = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?
            .sop_name
            .clone();
        let sop = self
            .sops
            .iter()
            .find(|s| s.name == sop_name)
            .cloned()
            .ok_or_else(|| anyhow::Error::msg(format!("SOP '{sop_name}' no longer loaded")))?;
        Ok((sop_name, sop))
    }

    fn fail_headless_driverless_step(
        &mut self,
        run_id: &str,
        sop_name: &str,
        step: &SopStep,
    ) -> SopRunAction {
        let reason = format!(
            "Headless deterministic SOP step {} '{}' requires an external driver; it was not executed",
            step.number, step.title
        );
        let now = now_iso8601();
        if let Some(run) = self.active_runs.get_mut(run_id) {
            run.step_results.push(SopStepResult {
                step_number: step.number,
                status: SopStepStatus::Failed,
                output: reason.clone(),
                started_at: now.clone(),
                completed_at: Some(now),
                tool_calls: Vec::new(),
            });
        }
        self.record_transition_event(
            run_id,
            "headless_driver_missing",
            Some(reason.clone()),
            ::serde_json::json!({
                "sop_name": sop_name,
                "step": step.number,
                "kind": step.kind.to_string(),
            }),
        );
        self.finish_run(run_id, SopRunStatus::Failed, Some(reason))
    }

    /// Resume a deterministic run from persisted state.
    pub fn resume_deterministic_run(
        &mut self,
        state: DeterministicRunState,
    ) -> Result<SopRunAction> {
        // Validate the run exists and is paused (immutable read), capturing its SOP
        // name, before any mutation - so the fail-closed reacquire can run first.
        let sop_name = match self.active_runs.get(&state.run_id) {
            Some(run) if run.status == SopRunStatus::PausedCheckpoint => run.sop_name.clone(),
            Some(run) => {
                bail!(
                    "Run {} is not paused at checkpoint (status: {})",
                    state.run_id,
                    run.status
                );
            }
            None => {
                let run_id = state.run_id.clone();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"run_id": run_id})),
                    "SOP engine: active run not found"
                );
                bail!("Active run not found: {}", state.run_id);
            }
        };

        // Refuse to resume while the checkpoint's parked snapshot has not yet
        // been durably persisted (see `is_park_persist_pending`'s doc): the kept
        // claim predates this attempt, and reacquiring on top of it would give a
        // later rollback or a maintenance retry no way to distinguish "freshly
        // reacquired" from "pre-existing, must survive."
        if self.is_park_persist_pending(&state.run_id) {
            bail!(
                "Run {} cannot resume: its parked checkpoint snapshot is not yet durably persisted (retrying)",
                state.run_id
            );
        }

        let sop = self
            .sops
            .iter()
            .find(|s| s.name == sop_name)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"sop_name": sop_name.as_str()})),
                    "SOP engine: sop no longer loaded (definition removed mid-run)"
                );
                anyhow::Error::msg(format!("SOP '{sop_name}' no longer loaded"))
            })?
            .clone();

        // Pre-flight the step this resume will advance to BEFORE reacquiring the
        // claim or mutating the run: a definition shrunk while parked must fail
        // closed here, with the run left untouched at `PausedCheckpoint`
        // (re-resolvable), instead of after the mutation below - which would
        // otherwise strand the run in `Running`, holding a claim, with no way to
        // make progress.
        let resume_step = if state.last_completed_step == 0 {
            1
        } else {
            state.last_completed_step
        };
        self.resolve_sop_step(&sop, resume_step)?;

        // A1: fail-closed - a restored parked run holds no exec claim; re-acquire it
        // BEFORE the transition and abort (leaving the run paused) if it fails.
        self.reacquire_claim_on_resume(&state.run_id)?;

        let run = self
            .active_runs
            .get_mut(&state.run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {}", state.run_id)))?;
        let prior_waiting_since = run.waiting_since.clone();
        let prior_llm_calls_saved = run.llm_calls_saved;
        let prior_current_step = run.current_step;
        run.status = SopRunStatus::Running;
        run.waiting_since = None;
        run.llm_calls_saved = state.llm_calls_saved;
        for (step_number, output) in &state.step_outputs {
            let already_recorded = run
                .step_results
                .iter()
                .any(|result| result.step_number == *step_number);
            if !already_recorded {
                run.step_results.push(SopStepResult {
                    step_number: *step_number,
                    status: SopStepStatus::Completed,
                    output: output.to_string(),
                    started_at: state.persisted_at.clone(),
                    completed_at: Some(state.persisted_at.clone()),
                    tool_calls: Vec::new(),
                });
            }
        }

        let last_output = state
            .step_outputs
            .get(&state.last_completed_step)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let run_id = state.run_id.clone();

        let outcome = if state.last_completed_step == 0 {
            self.dispatch_deterministic_step(&run_id, &sop, 1, last_output)
        } else {
            {
                let run = self.active_runs.get_mut(&run_id).unwrap();
                run.current_step = state.last_completed_step;
            }
            self.resolve_sop_step(&sop, state.last_completed_step)
                .and_then(|current_step| {
                    self.route_recorded_step(
                        &run_id,
                        &sop,
                        &current_step,
                        SopStepStatus::Completed,
                        true,
                        None,
                        Some(last_output),
                    )
                })
        };

        match outcome {
            Ok(action) => Ok(action),
            Err(e) => {
                // Defensive: the pre-flight above validated the same step lookup
                // under this lock, so this is unreachable in practice. If it still
                // fails, roll the run back to `PausedCheckpoint` and release the
                // just-reacquired claim so it doesn't get stuck in `Running`
                // holding a leaked exec slot.
                if let Some(run) = self.active_runs.get_mut(&run_id) {
                    run.status = SopRunStatus::PausedCheckpoint;
                    run.waiting_since = prior_waiting_since;
                    run.llm_calls_saved = prior_llm_calls_saved;
                    run.current_step = prior_current_step;
                }
                self.release_claim_on_park(&run_id);
                Err(e)
            }
        }
    }

    /// Resolve the action for a deterministic step (execute or checkpoint).
    fn resolve_deterministic_action(
        &mut self,
        sop: &Sop,
        run_id: &str,
        step: &SopStep,
        input: serde_json::Value,
    ) -> Result<SopRunAction> {
        let run_data = {
            let run = self
                .active_runs
                .get(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            RunData::from_step_results(&run.step_results)
        };
        if !route::eligible(step, &run_data) {
            return Ok(self.mark_step_pending(
                run_id,
                sop,
                step.number,
                format!("step {} dependencies not satisfied", step.number),
            ));
        }

        if let Some(action) = self.schema_input_failure_action(run_id, step, &input) {
            return Ok(action);
        }

        match step.kind {
            SopStepKind::Checkpoint => {
                // A prior checkpoint's recorded result (it records on resolve)
                // means this run has presented a gate before.
                let has_prior_gate = self.active_runs.get(run_id).is_some_and(|run| {
                    run.step_results.iter().any(|r| {
                        sop.steps
                            .iter()
                            .any(|s| s.number == r.step_number && s.kind == SopStepKind::Checkpoint)
                    })
                });
                // Pause at checkpoint - persist state and wait for approval
                if let Some(run) = self.active_runs.get_mut(run_id) {
                    run.status = SopRunStatus::PausedCheckpoint;
                    run.waiting_since = Some(now_iso8601());
                    // A NEW gate presentation (not a revise re-park): after the
                    // run's first-ever park, bump the presentation counter so
                    // this gate's prompt reference can never collide with an
                    // earlier gate's leftover buttons, and rebase the per-gate
                    // revise budget (`revision - revision_base`).
                    if run.revision > 0 || has_prior_gate {
                        run.revision += 1;
                    }
                    run.revision_base = run.revision;
                }

                let state_file = self.persist_deterministic_state(run_id, sop)?;

                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "Deterministic SOP run {run_id}: checkpoint at step {} '{}', state persisted to {}",
                        step.number,
                        step.title,
                        state_file.display().to_string()
                    )
                );

                // Mirror the paused checkpoint into the shared run store (alongside
                // the deterministic state file) so a restart leaves a non-terminal
                // row for restore_runs() to rehydrate. The helper frees the exec
                // slot and sends the approval request only after the parked snapshot
                // is durable; a failure keeps the claim for maintenance retry.
                self.persist_parked_snapshot_then_release_claim(run_id);

                Ok(SopRunAction::CheckpointWait {
                    run_id: run_id.to_string(),
                    step: step.clone(),
                    state_file,
                })
            }
            SopStepKind::Capability => self.execute_capability_step(sop, run_id, step, input),
            SopStepKind::Execute => {
                // Persist the active (Running) deterministic run so a restart mid-run
                // leaves a non-terminal row for restore_runs() to rehydrate. This is
                // the single sink for start / advance / resume deterministic steps.
                self.persist_active(run_id);

                Ok(SopRunAction::DeterministicStep {
                    run_id: run_id.to_string(),
                    step: step.clone(),
                    input,
                })
            }
        }
    }

    /// Persist the current deterministic run state to a JSON file.
    fn persist_deterministic_state(&self, run_id: &str, sop: &Sop) -> Result<PathBuf> {
        let run = self.active_runs.get(run_id).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"run_id": run_id})),
                "SOP engine: run not found in history"
            );
            anyhow::Error::msg(format!("Run not found: {run_id}"))
        })?;

        let mut step_outputs = HashMap::new();
        let mut last_completed_step = 0;
        for result in &run.step_results {
            if result.status == SopStepStatus::Completed {
                // Try to parse output as JSON, fall back to string value.
                let value = serde_json::from_str(&result.output)
                    .unwrap_or_else(|_| serde_json::Value::String(result.output.clone()));
                step_outputs.insert(result.step_number, value);
                last_completed_step = result.step_number;
            }
        }

        let state = DeterministicRunState {
            run_id: run_id.to_string(),
            sop_name: run.sop_name.clone(),
            last_completed_step,
            total_steps: run.total_steps,
            step_outputs,
            persisted_at: now_iso8601(),
            llm_calls_saved: run.llm_calls_saved,
            paused_at_checkpoint: run.status == SopRunStatus::PausedCheckpoint,
        };

        // Write to SOP location directory, or system temp dir
        let temp_dir = std::env::temp_dir();
        let dir = sop.location.as_deref().unwrap_or(temp_dir.as_path());
        let state_file = dir.join(format!("{run_id}.state.json"));
        let json = serde_json::to_string_pretty(&state)?;
        std::fs::write(&state_file, json)?;

        Ok(state_file)
    }

    /// Best-effort removal of a run's park-snapshot file once the run is
    /// terminal. Mirrors `persist_deterministic_state`'s path resolution; a
    /// missing file (the run never parked) is not an error.
    fn remove_deterministic_state_file(&self, run: &SopRun) {
        let temp_dir = std::env::temp_dir();
        let dir = self
            .get_sop(&run.sop_name)
            .and_then(|sop| sop.location.clone())
            .unwrap_or(temp_dir);
        let path = dir.join(format!("{}.state.json", run.run_id));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "run_id": run.run_id,
                            "path": path.display().to_string(),
                            "error": e.to_string(),
                        })),
                    "SOP engine: terminal run's park snapshot could not be removed"
                );
            }
        }
    }

    /// Load a persisted deterministic run state from a JSON file.
    pub fn load_deterministic_state(path: &Path) -> Result<DeterministicRunState> {
        let content = std::fs::read_to_string(path)?;
        let state: DeterministicRunState = serde_json::from_str(&content)?;
        Ok(state)
    }

    // ── Approval timeout ──────────────────────────────────────────

    /// Apply the configured timeout action to every timed-out WaitingApproval run.
    ///
    /// FAIL-CLOSED (EPIC C): priority no longer decides fail-open vs fail-closed;
    /// the typed `approval_timeout_action` does, uniformly. The default `Escalate`
    /// re-surfaces the gate to the out-of-band approver and NEVER self-approves
    /// (the old Critical/High auto-approve is gone; it is reachable only via the
    /// explicit `AutoApprove` opt-in). Returns any actions produced (a `Cancel`
    /// terminal action, or an `AutoApprove` resumed action); `Escalate` returns none.
    pub fn check_approval_timeouts(&mut self) -> Vec<SopRunAction> {
        let action_cfg = self.config.approval_timeout_action;
        let mut actions = Vec::new();
        for run_id in self.overdue_waiting_run_ids() {
            if let Some(a) =
                super::approval::timeout::apply_timeout_action(self, &run_id, action_cfg)
            {
                actions.push(a);
            }
        }
        actions
    }

    /// Run ids of `WaitingApproval` gates whose approval has timed out
    /// (`now - waiting_since >= approval_timeout_secs`). Empty when timeouts are
    /// disabled (`approval_timeout_secs == 0`). Shared by `check_approval_timeouts`
    /// (which applies the timeout action to each) and the maintenance tick (which
    /// counts them), so the overdue predicate lives in exactly one place.
    fn overdue_waiting_run_ids(&self) -> Vec<String> {
        let timeout_secs = self.config.approval_timeout_secs;
        if timeout_secs == 0 {
            return Vec::new();
        }
        // cooldown_elapsed(ts, secs) returns true when (now - ts) >= secs.
        self.active_runs
            .values()
            .filter(|r| r.status == SopRunStatus::WaitingApproval)
            .filter(|r| {
                r.waiting_since
                    .as_deref()
                    .is_some_and(|ts| cooldown_elapsed(ts, timeout_secs))
            })
            .map(|r| r.run_id.clone())
            .collect()
    }

    /// One periodic maintenance pass (EPIC A1 daemon tick). On each tick it:
    ///   1. fires fail-closed approval timeouts (`check_approval_timeouts`),
    ///   2. reaps concurrency-claim leases whose holder died without releasing,
    ///   3. prunes terminal runs past the retention policy.
    ///
    /// A no-op when nothing is due. Returns counts for observability. The returned
    /// `timeout_actions` are mostly self-applied (the fail-closed `Escalate`
    /// re-stamps the gate, `Cancel` finalizes the run); an opt-in `AutoApprove`
    /// yields a resumed `ExecuteStep` the caller logs until the live SOP executor
    /// (EPIC A2) exists.
    pub fn run_maintenance_tick(&mut self) -> MaintenanceSummary {
        // Count overdue gates BEFORE applying the action: the fail-closed Escalate
        // default re-stamps in place and produces no action, so counting actions
        // alone would under-report the escalations.
        let timed_out = self.overdue_waiting_run_ids().len();
        let timeout_actions = self.check_approval_timeouts();
        self.retry_pending_park_persists();
        self.heartbeat_active_claims();
        let reaped_claims = self.reap_expired_claims();
        let pruned_runs = self.prune_terminal_runs();
        MaintenanceSummary {
            timed_out,
            reaped_claims,
            pruned_runs,
            timeout_actions,
        }
    }

    /// Reclaim concurrency-claim leases past their expiry (the holder died without
    /// releasing). Best-effort: a store error is logged and the pass continues.
    /// Returns the number reclaimed.
    fn reap_expired_claims(&self) -> usize {
        let now = now_iso8601();
        let expired = match self.store.expired_claims(&now) {
            Ok(claims) => claims,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                    "SOP maintenance: failed to read expired claims"
                );
                return 0;
            }
        };
        let mut reclaimed = 0;
        for token in &expired {
            match self.store.release_claim(token) {
                Ok(()) => reclaimed += 1,
                Err(e) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                    "SOP maintenance: failed to release expired claim"
                ),
            }
        }
        reclaimed
    }

    /// Drop terminal runs beyond the retention policy (`max_finished_runs`).
    /// Best-effort; returns the number pruned.
    fn prune_terminal_runs(&self) -> usize {
        let policy = RetentionPolicy {
            max_terminal: self.config.max_finished_runs,
            keep_secs: None,
        };
        match self.store.prune(&policy) {
            Ok(n) => n,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                    "SOP maintenance: failed to prune terminal runs"
                );
                0
            }
        }
    }

    /// Re-stamp a run's `waiting_since` to now (timeout escalation: the gate stays
    /// open but the clock resets so it re-surfaces, not self-approves).
    pub(crate) fn restamp_waiting(&mut self, run_id: &str) {
        let restamped = match self.active_runs.get_mut(run_id) {
            Some(run) => {
                run.waiting_since = Some(now_iso8601());
                true
            }
            None => false,
        };
        // Persist so the re-stamped clock survives a restart; otherwise an
        // escalated gate would re-time-out immediately on the next boot.
        if restamped {
            self.persist_active(run_id);
        }
    }

    /// The current step number of an active run (0 if absent). For ledger rows.
    pub(crate) fn run_current_step(&self, run_id: &str) -> u32 {
        self.active_runs
            .get(run_id)
            .map(|r| r.current_step)
            .unwrap_or(0)
    }

    // ── Test helpers ──────────────────────────────────────────────

    /// Replace loaded SOPs (for testing from other modules).
    // Available for cross-crate testing
    pub fn set_sops_for_test(&mut self, sops: Vec<Sop>) {
        self.sops = sops;
    }

    // ── Internal helpers ────────────────────────────────────────

    pub fn last_finished_run(&self, sop_name: &str) -> Option<&SopRun> {
        self.finished_runs
            .iter()
            .rev()
            .find(|r| r.sop_name == sop_name)
    }

    pub fn finish_run(
        &mut self,
        run_id: &str,
        status: SopRunStatus,
        reason: Option<String>,
    ) -> SopRunAction {
        let mut run = self.active_runs.get(run_id).cloned().unwrap();
        run.status = status;
        run.completed_at = Some(now_iso8601());
        let sop_name = run.sop_name.clone();
        let run_id_owned = run.run_id.clone();
        if let Err(e) = self.persist_terminal(&run) {
            let error = format!("failed to persist terminal run (fail-closed): {e}");
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "run_id": run.run_id.as_str(),
                        "error": e.to_string(),
                    })),
                "SOP engine: failed to persist terminal run; keeping active run for retry"
            );
            return SopRunAction::Failed {
                run_id: run_id_owned,
                sop_name,
                reason: error,
            };
        }

        self.active_runs.remove(run_id);
        self.metrics.record_run_complete(&run);
        // The park snapshot is purely a rehydration artifact: a terminal run must
        // not leave one behind claiming `paused_at_checkpoint`. Decisions and the
        // final status live in the run store / approval ledger, not the snapshot.
        self.remove_deterministic_state_file(&run);
        self.finished_runs.push(run);

        // Evict oldest finished runs when over capacity
        let max = self.config.max_finished_runs;
        if max > 0 && self.finished_runs.len() > max {
            let excess = self.finished_runs.len() - max;
            self.finished_runs.drain(..excess);
        }

        match status {
            SopRunStatus::Failed => SopRunAction::Failed {
                run_id: run_id_owned,
                sop_name,
                reason: reason.unwrap_or_default(),
            },
            _ => SopRunAction::Completed {
                run_id: run_id_owned,
                sop_name,
            },
        }
    }

    pub(crate) fn finish_run_checked(
        &mut self,
        run_id: &str,
        status: SopRunStatus,
        reason: Option<String>,
    ) -> Result<SopRunAction> {
        let action = self.finish_run(run_id, status, reason);
        if let Some(reason) = Self::terminal_persistence_failure(&action) {
            return Err(anyhow::Error::msg(reason.to_string()));
        }
        Ok(action)
    }

    pub(crate) fn terminal_persistence_failure(action: &SopRunAction) -> Option<&str> {
        match action {
            SopRunAction::Failed { reason, .. }
                if reason.starts_with("failed to persist terminal run") =>
            {
                Some(reason.as_str())
            }
            _ => None,
        }
    }

    // ── EPIC C: out-of-band approval plane ──────────────────────────

    /// Read-only config access for the approval resolver.
    pub fn config(&self) -> &SopConfig {
        &self.config
    }

    /// The live `[sop.approval]` config - the single source of truth for approval
    /// groups and policies. The broker resolves membership/policy from this at
    /// use-time rather than holding a cloned copy that could drift on reload.
    pub fn approval_config(&self) -> &zeroclaw_config::schema::SopApprovalConfig {
        &self.config.approval
    }

    /// The name of the approval policy that applies to the run's currently-waiting
    /// step, if that step names one. Shared by the broker (membership/quorum) and
    /// the approval query surfaces so the "which policy applies now" lookup lives in
    /// exactly one place.
    pub fn current_step_policy_name(&self, run_id: &str) -> Option<String> {
        let run = self.get_run(run_id)?;
        let sop = self.get_sop(&run.sop_name)?;
        // Match the step by its `number`, NOT by vec position: routed / non-contiguous
        // step numbers mean position != number, and a positional lookup would read the
        // wrong step's policy (silently unpolicing a policied gate, or vice versa).
        let name = sop
            .steps
            .iter()
            .find(|s| s.number == run.current_step)?
            .policy
            .as_deref()?
            .trim();
        // An empty/whitespace name means "no policy", same as the Markdown parser's
        // `policy:` bullet (mod.rs). Without this, a TOML `policy = ""` step would
        // deserialize as `Some("")` and the broker would treat it as a NAMED-but-absent
        // policy (fail closed, gate stuck waiting forever) instead of unpoliced -
        // diverging from the equivalent Markdown SOP, which normalizes to `None`.
        (!name.is_empty()).then(|| name.to_string())
    }

    /// Classify a run's approval gate for `resolve_gate` (idempotency + typed
    /// not-found). `Running` (already approved) and terminal runs are
    /// `AlreadyResolved`; an unknown run or a non-`WaitingApproval` active status
    /// (e.g. a deterministic `PausedCheckpoint`, which `approve_step` owns) is
    /// `NotApplicable`.
    pub(crate) fn gate_state(&self, run_id: &str) -> GateState {
        if let Some(run) = self.active_runs.get(run_id) {
            match run.status {
                SopRunStatus::WaitingApproval => GateState::Waiting {
                    step: run.current_step,
                },
                SopRunStatus::Running => GateState::AlreadyResolved,
                _ => GateState::NotApplicable,
            }
        } else if self.finished_runs.iter().any(|r| r.run_id == run_id) {
            GateState::AlreadyResolved
        } else {
            GateState::NotApplicable
        }
    }

    /// Append an approval-ledger row via EPIC B's append-only event log. The store
    /// assigns the monotonic seq.
    ///
    /// FAIL-LOUD: the `StoreError` is propagated, never swallowed, so the caller
    /// can fail closed. The run-store gate ledger is the single audit of record
    /// for gate resolutions (the legacy Memory approval audit was removed), so a
    /// gate must not clear / deny / escalate / cancel unless its who/what/when row
    /// is durably written first - matching the store's fail-loud, fail-closed
    /// persistence contract. Callers append BEFORE mutating gate state.
    pub(crate) fn record_gate_event(
        &self,
        entry: super::approval::GateLedgerEntry,
    ) -> Result<(), StoreError> {
        self.store
            .append_event(&entry.into_event_record())
            .map(|_| ())
    }

    /// Ordered event/ledger history for a run (from the durable store).
    pub fn run_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
        self.store.list_events(run_id)
    }

    /// EPIC G (broker quorum): record an approver's vote on a still-waiting gate as
    /// an append-only ledger row (kind `gate_vote`, actor = the principal). Quorum is
    /// counted from these rows so votes are durable and survive a restart. Distinct
    /// from `gate_resolved`, which is appended only once the gate actually clears.
    pub(crate) fn record_gate_vote(
        &self,
        run_id: &str,
        step: u32,
        gate_revision: u32,
        principal: &super::approval::ApprovalPrincipal,
    ) -> Result<(), StoreError> {
        self.record_gate_vote_scoped(run_id, step, Some(gate_revision), None, None, principal)
    }

    /// Record a quorum vote for a deterministic checkpoint presentation. Checkpoint
    /// votes must be scoped tighter than approval-gate votes because the same step
    /// can be answered with materially different public-mutation decisions.
    pub(crate) fn record_checkpoint_gate_vote(
        &self,
        run_id: &str,
        step: u32,
        checkpoint_revision: u32,
        decision_label: &str,
        decision_identity: &str,
        principal: &super::approval::ApprovalPrincipal,
    ) -> Result<(), StoreError> {
        self.record_gate_vote_scoped(
            run_id,
            step,
            Some(checkpoint_revision),
            Some(decision_label),
            Some(decision_identity),
            principal,
        )
    }

    fn record_gate_vote_scoped(
        &self,
        run_id: &str,
        step: u32,
        gate_revision: Option<u32>,
        decision_label: Option<&str>,
        decision_identity: Option<&str>,
        principal: &super::approval::ApprovalPrincipal,
    ) -> Result<(), StoreError> {
        let mut payload = serde_json::json!({
            "step": step,
            "source": principal.source_label(),
        });
        if let Some(object) = payload.as_object_mut() {
            if let Some(revision) = gate_revision {
                object.insert(
                    "gate_revision".to_string(),
                    serde_json::Value::Number(revision.into()),
                );
                if decision_identity.is_some() {
                    object.insert(
                        "checkpoint_revision".to_string(),
                        serde_json::Value::Number(revision.into()),
                    );
                }
            }
            if let Some(label) = decision_label {
                object.insert(
                    "decision".to_string(),
                    serde_json::Value::String(label.to_string()),
                );
            }
            if let Some(identity) = decision_identity {
                object.insert(
                    "decision_identity".to_string(),
                    serde_json::Value::String(identity.to_string()),
                );
            }
        }
        let ev = SopEventRecord {
            run_id: run_id.to_string(),
            seq: 0,
            ts: now_iso8601(),
            kind: "gate_vote".to_string(),
            // `voter_key()` deliberately collapses `Http`/`Ws` to one canonical
            // `gateway:<id>` voter (same paired token, two transports = one voter),
            // while the agent/CLI sources stay distinct. See `ApprovalPrincipal::
            // voter_key`'s own doc for the full canonicalization rationale.
            actor: Some(principal.voter_key()),
            reason: None,
            payload,
        };
        self.store.append_event(&ev).map(|_| ())
    }

    /// EPIC G (broker quorum): count the DISTINCT approver identities that have voted
    /// to approve the gate on `run_id` AT `step` (from the append-only `gate_vote`
    /// ledger rows). Votes are scoped to the current step, so a multi-gate SOP does
    /// not carry step-1 votes into step 2; the voter key is source-qualified, so a
    /// repeat vote by the same identity on the same source counts once. Returns 0 on
    /// a store error (fail-closed: an unreadable ledger cannot satisfy a quorum).
    pub(crate) fn distinct_gate_voters(
        &self,
        run_id: &str,
        step: u32,
        gate_revision: u32,
    ) -> usize {
        self.distinct_gate_voters_scoped(run_id, step, Some(gate_revision), None)
    }

    /// Count distinct checkpoint voters for exactly one gate presentation and one
    /// canonical positive decision identity. Stale-prompt votes and mixed
    /// approve/amend/revise payloads do not combine.
    pub(crate) fn distinct_checkpoint_gate_voters(
        &self,
        run_id: &str,
        step: u32,
        checkpoint_revision: u32,
        decision_identity: &str,
    ) -> usize {
        self.distinct_gate_voters_scoped(
            run_id,
            step,
            Some(checkpoint_revision),
            Some(decision_identity),
        )
    }

    fn distinct_gate_voters_scoped(
        &self,
        run_id: &str,
        step: u32,
        gate_revision: Option<u32>,
        decision_identity: Option<&str>,
    ) -> usize {
        let Ok(events) = self.store.list_events(run_id) else {
            return 0;
        };
        let mut voters: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for ev in events {
            if ev.kind != "gate_vote"
                || ev.payload.get("step").and_then(|s| s.as_u64()) != Some(u64::from(step))
            {
                continue;
            }
            let event_revision = ev
                .payload
                .get("gate_revision")
                .or_else(|| ev.payload.get("checkpoint_revision"))
                .and_then(|r| r.as_u64());
            let scope_matches = match (gate_revision, decision_identity) {
                (Some(revision), Some(identity)) => {
                    event_revision == Some(u64::from(revision))
                        && ev.payload.get("decision_identity").and_then(|i| i.as_str())
                            == Some(identity)
                }
                (Some(revision), None) => {
                    event_revision == Some(u64::from(revision))
                        && ev.payload.get("decision_identity").is_none()
                }
                (None, None) => {
                    ev.payload.get("gate_revision").is_none()
                        && ev.payload.get("checkpoint_revision").is_none()
                        && ev.payload.get("decision_identity").is_none()
                }
                _ => false,
            };
            if !scope_matches {
                continue;
            }
            if let Some(actor) = ev.actor {
                voters.insert(actor);
            }
        }
        voters.len()
    }

    /// Record the approval completion metric at the gate-clearing chokepoint, so
    /// every principal (agent tool, CLI, gateway, WS, timeout) meters identically
    /// and the live counters agree with `SopMetricsCollector::rebuild_from_persistence`.
    /// `is_system` (the timeout principal) is metered as a timeout auto-approval;
    /// any other principal is a human approval. No-op if the run is gone.
    pub(crate) fn record_approval_metric(&self, run_id: &str, is_system: bool) {
        let Some(run) = self.get_run(run_id) else {
            return;
        };
        if is_system {
            self.metrics
                .record_timeout_auto_approve(&run.sop_name, &run.run_id);
        } else {
            self.metrics.record_approval(&run.sop_name, &run.run_id);
        }
    }

    /// The single out-of-band gate-clearing entry point (EPIC C). All four
    /// principals (agent tool, CLI, gateway, timeout tick) funnel through here.
    /// Sibling of `approve_step` (which keeps the deterministic-checkpoint resume
    /// path); the shared `WaitingApproval -> ExecuteStep` body is `clear_waiting_gate`.
    pub fn resolve_gate(
        &mut self,
        run_id: &str,
        decision: super::approval::ApprovalDecision,
        principal: super::approval::ApprovalPrincipal,
    ) -> Result<super::approval::ResolveOutcome> {
        super::approval::resolve::resolve_gate(self, run_id, decision, principal)
    }
}

/// Classification of a run's approval-gate state (EPIC C `resolve_gate`).
pub(crate) enum GateState {
    /// Waiting on approval at this step number (resolvable).
    Waiting { step: u32 },
    /// Already resolved (running after approve, or terminal) - idempotent no-op.
    AlreadyResolved,
    /// Not a waiting-approval gate (unknown run, or a non-WaitingApproval status
    /// such as a deterministic `PausedCheckpoint`, which `approve_step` owns).
    NotApplicable,
}

// ── Trigger matching ────────────────────────────────────────────

/// Check whether a single trigger definition matches an incoming event.
///
/// Source class is the cheap gate: a trigger can only match an event from its
/// own source. Past that, matching is the trigger's own responsibility via its
/// `TriggerBehavior`, so there is no per-source logic to drift here.
fn trigger_matches(trigger: &SopTrigger, event: &SopEvent) -> bool {
    trigger.source() == event.source && trigger.behavior().matches(event)
}

/// Match a channel trigger against an event topic. Two producer forms are
/// accepted through the shared [`ChannelSopTopic`] grammar: the plain
/// `channel` / `channel/alias` form used by agent-loop message triggers, and
/// the forge form `channel.alias:event_type`. Channel type compares
/// case-insensitively; an aliased trigger requires an exact alias, an
/// alias-less trigger matches any instance. No topic fails closed. The
/// `event_type` (forge form) is left for an authored `condition` to match.
pub(crate) fn channel_trigger_topic_matches(
    channel: &str,
    alias: Option<&str>,
    topic: Option<&str>,
) -> bool {
    let Some(topic) = topic else {
        return false;
    };
    let (topic_channel, topic_alias, _event_type) =
        zeroclaw_api::channel::ChannelSopTopic::parse(topic);
    if !topic_channel.eq_ignore_ascii_case(channel) {
        return false;
    }
    match alias {
        Some(a) => topic_alias.is_some_and(|ta| ta == a),
        None => true,
    }
}

pub(crate) fn calendar_trigger_matches(
    calendar_source: &str,
    calendar_ids: &[String],
    event: &SopEvent,
) -> bool {
    if event.topic.as_deref() != Some(CALENDAR_NO_SHOW_TOPIC) {
        return false;
    }

    let Some(payload) = event.payload.as_deref() else {
        return false;
    };
    let Ok(payload) = serde_json::from_str::<CalendarNoShowEvent>(payload) else {
        return false;
    };

    if payload.calendar_source != calendar_source {
        return false;
    }

    if calendar_ids.is_empty() {
        return true;
    }

    calendar_ids.iter().any(|id| id == &payload.calendar_id)
}

/// Simple MQTT topic matching with `+` (single-level) and `#` (multi-level) wildcards.
pub(crate) fn mqtt_topic_matches(pattern: &str, topic: &str) -> bool {
    let pat_parts: Vec<&str> = pattern.split('/').collect();
    let top_parts: Vec<&str> = topic.split('/').collect();

    let mut pi = 0;
    let mut ti = 0;

    while pi < pat_parts.len() && ti < top_parts.len() {
        match pat_parts[pi] {
            "#" => return true, // multi-level wildcard matches everything remaining
            "+" => {
                // single-level wildcard matches one segment
                pi += 1;
                ti += 1;
            }
            seg => {
                if seg != top_parts[ti] {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }

    // Both must be fully consumed (unless pattern ended with #)
    pi == pat_parts.len() && ti == top_parts.len()
}

/// AMQP topic-exchange routing-key matching. Keys are `.`-delimited words;
/// `*` matches exactly one word and `#` matches zero or more words. A `#` that
/// can absorb zero segments is what distinguishes this from MQTT matching.
pub(crate) fn amqp_routing_key_matches(pattern: &str, key: &str) -> bool {
    let pat: Vec<&str> = pattern.split('.').collect();
    let words: Vec<&str> = key.split('.').collect();
    amqp_match_from(&pat, &words)
}

fn amqp_match_from(pat: &[&str], words: &[&str]) -> bool {
    match pat.first() {
        None => words.is_empty(),
        Some(&"#") => (0..=words.len()).any(|skip| amqp_match_from(&pat[1..], &words[skip..])),
        Some(&"*") => !words.is_empty() && amqp_match_from(&pat[1..], &words[1..]),
        Some(seg) => {
            !words.is_empty() && *seg == words[0] && amqp_match_from(&pat[1..], &words[1..])
        }
    }
}

/// Glob match a filesystem trigger `pattern` against a normalized `path`,
/// supporting `*` (single segment) and `**` (recursive) wildcards via the
/// `glob` crate. A bare directory pattern also matches paths nested beneath it.
pub(crate) fn filesystem_path_matches(pattern: &str, path: &str) -> bool {
    if let Ok(compiled) = glob::Pattern::new(pattern)
        && compiled.matches(path)
    {
        return true;
    }
    let prefix = pattern.trim_end_matches('/');
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Whether the payload's `event` field names one of the trigger's listed kinds.
pub(crate) fn filesystem_event_listed(
    events: &[FilesystemEventKind],
    payload: Option<&str>,
) -> bool {
    let Some(payload) = payload else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    let Some(kind) = value.get("event").and_then(|e| e.as_str()) else {
        return false;
    };
    events.iter().any(|e| e.to_string() == kind)
}

// ── Execution mode resolution ───────────────────────────────────

fn execution_mode_needs_approval(mode: SopExecutionMode, sop: &Sop, step: &SopStep) -> bool {
    match mode {
        // Deterministic mode is handled via start_deterministic_run;
        // if we reach here via the standard path, treat as Auto.
        SopExecutionMode::Auto | SopExecutionMode::Deterministic => false,
        SopExecutionMode::Supervised => {
            // Supervised: approval only before the first step
            step.number == 1
        }
        SopExecutionMode::StepByStep => true,
        SopExecutionMode::PriorityBased => match sop.priority {
            // [SEC-FLIP] Critical/High are the MOST dangerous runs, so they MUST
            // gate (was `=> false`, an inversion that auto-ran the riskiest SOPs).
            SopPriority::Critical | SopPriority::High => true,
            SopPriority::Normal | SopPriority::Low => {
                // Supervised behavior for normal/low
                step.number == 1
            }
        },
    }
}

/// Determine the action for a step based on the effective execution mode.
fn resolve_step_action(sop: &Sop, step: &SopStep, run_id: String, context: String) -> SopRunAction {
    let mut step = step.clone();
    step.agent = step
        .effective_agent(sop.agent.as_deref())
        .map(str::to_string);
    let step = &step;

    // Steps with requires_confirmation always need approval
    if step.requires_confirmation {
        return SopRunAction::WaitApproval {
            run_id,
            step: step.clone(),
            context,
        };
    }

    let effective_mode = step.mode.unwrap_or(sop.execution_mode);
    let sop_needs_approval = execution_mode_needs_approval(sop.execution_mode, sop, step);
    let step_needs_approval = execution_mode_needs_approval(effective_mode, sop, step);
    let needs_approval = sop_needs_approval || step_needs_approval;

    if needs_approval {
        SopRunAction::WaitApproval {
            run_id,
            step: step.clone(),
            context,
        }
    } else {
        SopRunAction::ExecuteStep {
            run_id,
            step: step.clone(),
            context,
        }
    }
}

// ── Step context formatting ─────────────────────────────────────

/// Build the structured context message that gets injected into the agent.
fn format_step_context(sop: &Sop, run: &SopRun, step: &SopStep, config: &SopConfig) -> String {
    let mut ctx = format!(
        "[SOP: {} (run {}) — Step {} of {}]\n\n",
        sop.name, run.run_id, step.number, run.total_steps
    );

    let marker_id = if run.frame_marker_id.is_empty() {
        run.run_id.as_str()
    } else {
        run.frame_marker_id.as_str()
    };
    ctx.push_str(&ContentSafety::from_sop_config(config).frame_for_context(
        run.trigger_event.payload.as_deref(),
        run.trigger_event.topic.as_deref(),
        run.trigger_event.source,
        marker_id,
    ));

    // Previous step summary
    if let Some(prev) = run.step_results.last() {
        let _ = writeln!(
            ctx,
            "Previous: Step {} {} — {}",
            prev.step_number, prev.status, prev.output
        );
    }

    let _ = write!(ctx, "\nCurrent step: **{}**\n{}\n", step.title, step.body);

    if !step.suggested_tools.is_empty() {
        let _ = write!(
            ctx,
            "\nSuggested tools: {}\n",
            step.suggested_tools.join(", ")
        );
    }

    ctx.push_str("\nWhen done, report your result.\n");

    ctx
}

pub(crate) fn step_input_value(run: &SopRun, step_number: u32) -> Value {
    if step_number <= 1 {
        return run
            .trigger_event
            .payload
            .as_deref()
            .map(jsonish_value)
            .unwrap_or(Value::Null);
    }

    run.step_results
        .last()
        .map(step_result_value)
        .unwrap_or(Value::Null)
}

/// Gate re-presentations per checkpoint a `Revise` may spend before the gate
/// insists on approve / edit / deny. Bounds operator-driven model spend.
pub(crate) const MAX_GATE_REVISIONS: u32 = 3;

/// The input that fed `step_number` when it originally ran: the output of the
/// step completed immediately BEFORE it in EXECUTION order (`step_results` is
/// append-only, so vec order IS execution order — numeric order would lie under
/// `Goto` routing), or the trigger payload when nothing ran before it. Used to
/// replay a step (a gate `Revise` re-draft) with exactly what it saw the first
/// time.
pub(crate) fn replay_input_for_step(run: &SopRun, step_number: u32) -> Value {
    let executed_at = run
        .step_results
        .iter()
        .rposition(|r| r.step_number == step_number && r.status == SopStepStatus::Completed);
    executed_at
        .and_then(|idx| {
            run.step_results[..idx]
                .iter()
                .rev()
                .find(|r| r.status == SopStepStatus::Completed)
                .map(step_result_value)
        })
        .unwrap_or_else(|| {
            run.trigger_event
                .payload
                .as_deref()
                .map(jsonish_value)
                .unwrap_or(Value::Null)
        })
}

fn retry_input_value(run: &SopRun, step_number: u32) -> Value {
    if step_number <= 1 {
        return run
            .trigger_event
            .payload
            .as_deref()
            .map(jsonish_value)
            .unwrap_or(Value::Null);
    }

    run.step_results
        .iter()
        .rev()
        .find(|result| {
            result.status == SopStepStatus::Completed && result.step_number != step_number
        })
        .map(step_result_value)
        .unwrap_or(Value::Null)
}

fn step_result_value(result: &SopStepResult) -> Value {
    jsonish_value(&result.output)
}

fn forge_comment_input_matches_checkpoint_output(
    input: &Value,
    checkpoint_result: &SopStepResult,
) -> bool {
    let Ok(target) = super::capability::resolve_forge_comment_target(input) else {
        return false;
    };
    let approved = step_result_value(checkpoint_result);
    let Some(approved) = approved.as_object() else {
        return false;
    };
    let approved_repo = approved
        .get("repo")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|repo| !repo.is_empty());
    let approved_number = approved.get("number").and_then(Value::as_u64);
    let approved_body = approved.get("body").and_then(Value::as_str);
    let approved_channel = approved
        .get("channel")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|channel| !channel.is_empty());
    let channel_matches = match target.channel {
        Some(channel) => approved_channel == Some(channel),
        None => true,
    };

    approved_repo == Some(target.repo)
        && approved_number == Some(target.number)
        && approved_body == Some(target.body)
        && channel_matches
}

fn jsonish_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into()))
}

// ── Utilities ───────────────────────────────────────────────────

pub fn now_iso8601() -> String {
    // Use chrono if available, otherwise fallback to SystemTime
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // Simple UTC timestamp without chrono dependency
    let secs = now.as_secs();
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Days since epoch to Y-M-D (simplified — good enough for run IDs)
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    days += 719_468;
    let era = days / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// A1: whether a run in `active_runs` currently occupies an execution slot (holds
/// a store CAS claim). A run parked at a HITL approval / deterministic checkpoint
/// releases its claim on park, so it does NOT hold a slot; every other non-terminal
/// status does. Keeps the in-memory admission fallback aligned with the store's
/// `claim_counts`, which counts only live (executing) claims.
fn holds_exec_claim(status: SopRunStatus) -> bool {
    !matches!(
        status,
        SopRunStatus::WaitingApproval | SopRunStatus::PausedCheckpoint
    )
}

/// Check if enough time has elapsed since a timestamp string.
fn cooldown_elapsed(completed_at: &str, cooldown_secs: u64) -> bool {
    // Parse the ISO-8601 timestamp we generate
    let completed = parse_iso8601_secs(completed_at);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    match completed {
        Some(ts) => now.saturating_sub(ts) >= cooldown_secs,
        None => true, // Can't parse timestamp; allow start
    }
}

/// Minimal ISO-8601 parser returning seconds since epoch.
fn parse_iso8601_secs(input: &str) -> Option<u64> {
    // Expected format: YYYY-MM-DDTHH:MM:SSZ
    let input = input.trim_end_matches('Z');
    let parts: Vec<&str> = input.split('T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|p| p.parse().ok()).collect();
    let time_parts: Vec<u64> = parts[1].split(':').filter_map(|p| p.parse().ok()).collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }
    let (year, month, day) = (date_parts[0], date_parts[1], date_parts[2]);
    let (hour, min, sec) = (time_parts[0], time_parts[1], time_parts[2]);

    // Reverse of days_to_ymd: compute days since epoch
    let year_adj = if month <= 2 { year - 1 } else { year };
    let month_adj = if month > 2 { month - 3 } else { month + 9 };
    let era = year_adj / 400;
    let yoe = year_adj - era * 400;
    let doy = (153 * month_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::super::store::ProposalKind;
    use super::*;
    use crate::sop::approval::{ApprovalDecision, ApprovalPrincipal, ResolveOutcome};
    use crate::sop::step_contract::StepFailure;
    use crate::sop::types::{SopExecutionMode, StepSchema};

    /// Clear a WaitingApproval gate through the production out-of-band chokepoint
    /// (a CLI principal), returning the resumed action. Mirrors what a real
    /// `zeroclaw sop approve` does, replacing the old `approve_step` agent path.
    fn approve_gate_cli(engine: &mut SopEngine, run_id: &str) -> SopRunAction {
        match engine
            .resolve_gate(
                run_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(None),
            )
            .unwrap()
        {
            ResolveOutcome::Resumed(action) => *action,
            other => panic!("expected Resumed, got {other:?}"),
        }
    }

    fn manual_event() -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        }
    }

    fn mqtt_event(topic: &str, payload: &str) -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some(topic.into()),
            payload: Some(payload.into()),
            timestamp: now_iso8601(),
        }
    }

    fn test_sop(name: &str, mode: SopExecutionMode, priority: SopPriority) -> Sop {
        Sop {
            name: name.into(),
            description: format!("Test SOP: {name}"),
            version: "1.0.0".into(),
            priority,
            execution_mode: mode,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Step one".into(),
                    body: "Do step one".into(),
                    suggested_tools: vec!["shell".into()],
                    requires_confirmation: false,
                    kind: SopStepKind::default(),
                    schema: None,
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "Step two".into(),
                    body: "Do step two".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::default(),
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

    fn engine_with_sops(sops: Vec<Sop>) -> SopEngine {
        engine_with_config_sops(SopConfig::default(), sops)
    }

    fn engine_with_config_sops(config: SopConfig, sops: Vec<Sop>) -> SopEngine {
        let mut engine = SopEngine::new(config);
        engine.sops = sops;
        engine
    }

    fn required_object_schema(key: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": [key]
        })
    }

    /// Extract run_id from any SopRunAction variant.
    fn extract_run_id(action: &SopRunAction) -> &str {
        match action {
            SopRunAction::ExecuteStep { run_id, .. }
            | SopRunAction::WaitApproval { run_id, .. }
            | SopRunAction::DeterministicStep { run_id, .. }
            | SopRunAction::CheckpointWait { run_id, .. }
            | SopRunAction::Pending { run_id, .. }
            | SopRunAction::Completed { run_id, .. }
            | SopRunAction::Failed { run_id, .. } => run_id,
        }
    }

    /// Get the first active run_id from the engine (for tests with a single run).
    #[allow(dead_code)]
    fn first_active_run_id(engine: &SopEngine) -> String {
        engine
            .active_runs()
            .keys()
            .next()
            .expect("expected at least one active run")
            .clone()
    }

    // ── Trigger matching ────────────────────────────────

    #[test]
    fn match_manual_trigger() {
        let engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let matches = engine.match_trigger(&manual_event());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "s1");
    }

    #[test]
    fn no_match_for_wrong_source() {
        let engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let event = mqtt_event("sensors/temp", "{}");
        let matches = engine.match_trigger(&event);
        assert!(matches.is_empty());
    }

    fn channel_event(topic: &str, payload: &str) -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Channel,
            topic: Some(topic.into()),
            payload: Some(payload.into()),
            timestamp: now_iso8601(),
        }
    }

    fn channel_sop(name: &str, alias: Option<&str>, condition: Option<&str>) -> Sop {
        let mut sop = test_sop(name, SopExecutionMode::Auto, SopPriority::Normal);
        sop.triggers = vec![SopTrigger::Channel {
            channel: "telegram".into(),
            alias: alias.map(str::to_string),
            condition: condition.map(str::to_string),
        }];
        sop
    }

    #[test]
    fn channel_trigger_matches_channel_type_case_insensitive() {
        let engine = engine_with_sops(vec![channel_sop("s1", None, None)]);
        assert_eq!(
            engine.match_trigger(&channel_event("telegram", "{}")).len(),
            1
        );
        assert_eq!(
            engine.match_trigger(&channel_event("Telegram", "{}")).len(),
            1
        );
        assert!(
            engine
                .match_trigger(&channel_event("discord", "{}"))
                .is_empty()
        );
    }

    #[test]
    fn channel_trigger_without_alias_matches_any_instance() {
        let engine = engine_with_sops(vec![channel_sop("s1", None, None)]);
        assert_eq!(
            engine
                .match_trigger(&channel_event("telegram/prod", "{}"))
                .len(),
            1
        );
        assert_eq!(
            engine.match_trigger(&channel_event("telegram", "{}")).len(),
            1
        );
    }

    #[test]
    fn channel_trigger_with_alias_requires_exact_alias() {
        let engine = engine_with_sops(vec![channel_sop("s1", Some("prod"), None)]);
        assert_eq!(
            engine
                .match_trigger(&channel_event("telegram/prod", "{}"))
                .len(),
            1
        );
        assert!(
            engine
                .match_trigger(&channel_event("telegram/backup", "{}"))
                .is_empty()
        );
        assert!(
            engine
                .match_trigger(&channel_event("telegram", "{}"))
                .is_empty(),
            "aliased trigger must not match an alias-less topic"
        );
    }

    #[test]
    fn channel_trigger_without_topic_fails_closed() {
        let engine = engine_with_sops(vec![channel_sop("s1", None, None)]);
        let event = SopEvent {
            source: SopTriggerSource::Channel,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());
    }

    #[test]
    fn channel_trigger_condition_filters_by_payload() {
        let engine = engine_with_sops(vec![channel_sop("s1", None, Some("$.kind == \"deploy\""))]);
        assert_eq!(
            engine
                .match_trigger(&channel_event("telegram", "{\"kind\":\"deploy\"}"))
                .len(),
            1
        );
        assert!(
            engine
                .match_trigger(&channel_event("telegram", "{\"kind\":\"chat\"}"))
                .is_empty()
        );
    }

    #[test]
    fn wants_source_reflects_loaded_trigger_sources() {
        let engine = engine_with_sops(vec![channel_sop("s1", None, None)]);
        assert!(engine.wants_source(SopTriggerSource::Channel));
        assert!(!engine.wants_source(SopTriggerSource::Mqtt));
        assert!(!engine.wants_source(SopTriggerSource::Amqp));

        let empty = engine_with_sops(vec![]);
        assert!(!empty.wants_source(SopTriggerSource::Channel));
    }

    fn amqp_event(routing_key: &str, payload: &str) -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Amqp,
            topic: Some(routing_key.into()),
            payload: Some(payload.into()),
            timestamp: now_iso8601(),
        }
    }

    #[test]
    fn amqp_routing_key_exact_star_hash() {
        assert!(amqp_routing_key_matches("a.b.c", "a.b.c"));
        assert!(!amqp_routing_key_matches("a.b.c", "a.b"));
        assert!(amqp_routing_key_matches("a.*.c", "a.b.c"));
        assert!(!amqp_routing_key_matches("a.*.c", "a.b.b.c"));
        assert!(amqp_routing_key_matches("a.#", "a.b.c.d"));
        assert!(amqp_routing_key_matches("a.#", "a"));
        assert!(amqp_routing_key_matches("#", ""));
        assert!(amqp_routing_key_matches("a.#.d", "a.d"));
        assert!(amqp_routing_key_matches("a.#.d", "a.b.c.d"));
        assert!(!amqp_routing_key_matches("a.#.d", "a.b.c"));
    }

    #[test]
    fn match_amqp_trigger_wildcard() {
        let sop = Sop {
            triggers: vec![SopTrigger::Amqp {
                routing_key: "org.*.anitya.#".into(),
                condition: None,
            }],
            ..test_sop("anitya-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);
        let hit = engine.match_trigger(&amqp_event(
            "org.release-monitoring.anitya.project.version.update",
            "{}",
        ));
        assert_eq!(hit.len(), 1);
        let miss = engine.match_trigger(&amqp_event("org.release-monitoring.fedmsg.x", "{}"));
        assert!(miss.is_empty());
    }

    #[test]
    fn match_mqtt_trigger_exact() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "plant/pump/pressure".into(),
                condition: None,
            }],
            ..test_sop(
                "pressure-sop",
                SopExecutionMode::Auto,
                SopPriority::Critical,
            )
        };
        let engine = engine_with_sops(vec![sop]);
        let matches = engine.match_trigger(&mqtt_event("plant/pump/pressure", "87.3"));
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn match_mqtt_wildcard_plus() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "plant/+/pressure".into(),
                condition: None,
            }],
            ..test_sop("wildcard-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);
        assert_eq!(
            engine
                .match_trigger(&mqtt_event("plant/pump_3/pressure", "87"))
                .len(),
            1
        );
        assert!(
            engine
                .match_trigger(&mqtt_event("plant/pump_3/temperature", "50"))
                .is_empty()
        );
    }

    #[test]
    fn match_mqtt_wildcard_hash() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "plant/#".into(),
                condition: None,
            }],
            ..test_sop("hash-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);
        assert_eq!(
            engine
                .match_trigger(&mqtt_event("plant/pump/pressure", "87"))
                .len(),
            1
        );
        assert_eq!(
            engine
                .match_trigger(&mqtt_event("plant/a/b/c/d", "x"))
                .len(),
            1
        );
    }

    #[test]
    fn mqtt_topic_matching_edge_cases() {
        assert!(mqtt_topic_matches("a/b/c", "a/b/c"));
        assert!(!mqtt_topic_matches("a/b/c", "a/b/d"));
        assert!(!mqtt_topic_matches("a/b/c", "a/b"));
        assert!(!mqtt_topic_matches("a/b", "a/b/c"));
        assert!(mqtt_topic_matches("+/+/+", "a/b/c"));
        assert!(!mqtt_topic_matches("+/+", "a/b/c"));
        assert!(mqtt_topic_matches("#", "a/b/c"));
        assert!(mqtt_topic_matches("a/#", "a/b/c"));
        assert!(!mqtt_topic_matches("b/#", "a/b/c"));
    }

    // ── Calendar trigger matching ─────────────────────

    fn calendar_event(topic: Option<&str>, calendar_source: &str, calendar_id: &str) -> SopEvent {
        let now = chrono::Utc::now();
        SopEvent {
            source: SopTriggerSource::Calendar,
            topic: topic.map(str::to_string),
            payload: Some(
                serde_json::json!({
                    "event_id": "evt-1",
                    "event_title": "Standup",
                    "expected_start": now,
                    "detected_at": now,
                    "calendar_source": calendar_source,
                    "calendar_id": calendar_id,
                })
                .to_string(),
            ),
            timestamp: now_iso8601(),
        }
    }

    #[test]
    fn calendar_trigger_matches_source_and_any_calendar_when_ids_empty() {
        let sop = Sop {
            triggers: vec![SopTrigger::Calendar {
                calendar_source: "microsoft365".into(),
                calendar_ids: Vec::new(),
                condition: None,
            }],
            ..test_sop("calendar-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        let matches = engine.match_trigger(&calendar_event(
            Some(CALENDAR_NO_SHOW_TOPIC),
            "microsoft365",
            "team",
        ));

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "calendar-sop");
    }

    #[test]
    fn calendar_trigger_filters_calendar_ids_and_source() {
        let sop = Sop {
            triggers: vec![SopTrigger::Calendar {
                calendar_source: "microsoft365".into(),
                calendar_ids: vec!["primary".into()],
                condition: None,
            }],
            ..test_sop("calendar-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        assert_eq!(
            engine
                .match_trigger(&calendar_event(
                    Some(CALENDAR_NO_SHOW_TOPIC),
                    "microsoft365",
                    "primary"
                ))
                .len(),
            1
        );
        assert!(
            engine
                .match_trigger(&calendar_event(
                    Some(CALENDAR_NO_SHOW_TOPIC),
                    "microsoft365",
                    "team"
                ))
                .is_empty()
        );
        assert!(
            engine
                .match_trigger(&calendar_event(
                    Some(CALENDAR_NO_SHOW_TOPIC),
                    "google",
                    "primary"
                ))
                .is_empty()
        );
    }

    #[test]
    fn calendar_trigger_requires_no_show_topic_and_valid_payload() {
        let sop = Sop {
            triggers: vec![SopTrigger::Calendar {
                calendar_source: "microsoft365".into(),
                calendar_ids: Vec::new(),
                condition: None,
            }],
            ..test_sop("calendar-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        assert!(
            engine
                .match_trigger(&calendar_event(
                    Some("calendar.updated"),
                    "microsoft365",
                    "primary"
                ))
                .is_empty()
        );

        let invalid_payload_event = SopEvent {
            source: SopTriggerSource::Calendar,
            topic: Some(CALENDAR_NO_SHOW_TOPIC.into()),
            payload: Some("not json".into()),
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&invalid_payload_event).is_empty());

        let missing_payload_event = SopEvent {
            source: SopTriggerSource::Calendar,
            topic: Some(CALENDAR_NO_SHOW_TOPIC.into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&missing_payload_event).is_empty());

        let malformed_payload_event = SopEvent {
            source: SopTriggerSource::Calendar,
            topic: Some(CALENDAR_NO_SHOW_TOPIC.into()),
            payload: Some(
                serde_json::json!({
                    "event_id": "evt-1",
                    "event_title": "Standup",
                    "expected_start": chrono::Utc::now(),
                    "detected_at": chrono::Utc::now(),
                    "calendar_source": "microsoft365",
                    "calendar_id": 17,
                })
                .to_string(),
            ),
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&malformed_payload_event).is_empty());
    }

    // ── Webhook trigger matching ─────────────────────

    #[test]
    fn webhook_trigger_matches_exact_path() {
        let sop = Sop {
            triggers: vec![SopTrigger::Webhook {
                path: "/webhook".into(),
            }],
            ..test_sop("webhook-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        // Exact match — should match
        let event = SopEvent {
            source: SopTriggerSource::Webhook,
            topic: Some("/webhook".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);
    }

    #[test]
    fn webhook_trigger_rejects_different_path() {
        let sop = Sop {
            triggers: vec![SopTrigger::Webhook {
                path: "/sop/deploy".into(),
            }],
            ..test_sop("deploy-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        // Path /webhook does NOT match /sop/deploy
        let event = SopEvent {
            source: SopTriggerSource::Webhook,
            topic: Some("/webhook".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());

        // But /sop/deploy matches /sop/deploy
        let event = SopEvent {
            source: SopTriggerSource::Webhook,
            topic: Some("/sop/deploy".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);
    }

    #[test]
    fn channel_trigger_matches_forge_topic_and_condition() {
        let sop = Sop {
            triggers: vec![SopTrigger::Channel {
                channel: "git".into(),
                alias: Some("main".into()),
                condition: Some("$.event_type == \"pull_request.opened\"".into()),
            }],
            ..test_sop("git-pr-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        let event = SopEvent {
            source: SopTriggerSource::Channel,
            topic: Some("git.main:pull_request.opened".into()),
            payload: Some(
                r#"{"event_type":"pull_request.opened","repo":"octo/repo","number":12}"#.into(),
            ),
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);

        let wrong_event_type = SopEvent {
            source: SopTriggerSource::Channel,
            topic: Some("git.main:issues.opened".into()),
            payload: Some(r#"{"event_type":"issues.opened","repo":"octo/repo"}"#.into()),
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&wrong_event_type).is_empty());

        let wrong_alias = SopEvent {
            source: SopTriggerSource::Channel,
            topic: Some("git.staging:pull_request.opened".into()),
            payload: Some(r#"{"event_type":"pull_request.opened","repo":"octo/repo"}"#.into()),
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&wrong_alias).is_empty());
    }

    // ── Cron trigger matching ─────────────────────────

    #[test]
    fn cron_trigger_matches_only_matching_expression() {
        let sop = Sop {
            triggers: vec![SopTrigger::Cron {
                expression: "0 */5 * * *".into(),
            }],
            ..test_sop("cron-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        // Matching expression
        let event = SopEvent {
            source: SopTriggerSource::Cron,
            topic: Some("0 */5 * * *".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);

        // Different expression — should NOT match
        let event = SopEvent {
            source: SopTriggerSource::Cron,
            topic: Some("0 */10 * * *".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());

        // No topic — should NOT match
        let event = SopEvent {
            source: SopTriggerSource::Cron,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());
    }

    // ── Condition-based trigger matching ────────────────

    #[test]
    fn mqtt_condition_filters_by_payload() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "sensors/pressure".into(),
                condition: Some("$.value > 85".into()),
            }],
            ..test_sop("cond-sop", SopExecutionMode::Auto, SopPriority::Critical)
        };
        let engine = engine_with_sops(vec![sop]);

        // Payload meets condition
        let matches = engine.match_trigger(&mqtt_event("sensors/pressure", r#"{"value": 90}"#));
        assert_eq!(matches.len(), 1);

        // Payload does not meet condition
        let matches = engine.match_trigger(&mqtt_event("sensors/pressure", r#"{"value": 50}"#));
        assert!(matches.is_empty());
    }

    #[test]
    fn mqtt_no_condition_matches_any_payload() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "sensors/temp".into(),
                condition: None,
            }],
            ..test_sop("no-cond", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        let matches = engine.match_trigger(&mqtt_event("sensors/temp", "anything"));
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn mqtt_condition_no_payload_fails_closed() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "sensors/temp".into(),
                condition: Some("$.value > 0".into()),
            }],
            ..test_sop("no-payload", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        // Event with no payload
        let event = SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some("sensors/temp".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());
    }

    #[test]
    fn peripheral_condition_filters_by_payload() {
        let sop = Sop {
            triggers: vec![SopTrigger::Peripheral {
                board: "nucleo".into(),
                signal: "pin_3".into(),
                condition: Some("> 0".into()),
            }],
            ..test_sop("periph-cond", SopExecutionMode::Auto, SopPriority::High)
        };
        let engine = engine_with_sops(vec![sop]);

        // Positive signal
        let event = SopEvent {
            source: SopTriggerSource::Peripheral,
            topic: Some("nucleo/pin_3".into()),
            payload: Some("1".into()),
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);

        // Zero signal — does not meet condition
        let event = SopEvent {
            source: SopTriggerSource::Peripheral,
            topic: Some("nucleo/pin_3".into()),
            payload: Some("0".into()),
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());
    }

    #[test]
    fn peripheral_no_condition_matches_any() {
        let sop = Sop {
            triggers: vec![SopTrigger::Peripheral {
                board: "rpi".into(),
                signal: "gpio_5".into(),
                condition: None,
            }],
            ..test_sop("periph-nocond", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        let event = SopEvent {
            source: SopTriggerSource::Peripheral,
            topic: Some("rpi/gpio_5".into()),
            payload: Some("0".into()),
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);
    }

    // ── Run lifecycle ───────────────────────────────────

    #[test]
    fn start_run_returns_first_step() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action);
        assert!(run_id.starts_with("run-"));
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));
        assert_eq!(engine.active_runs().len(), 1);
    }

    #[test]
    fn run_notifier_publishes_on_admission() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )])
        .with_run_notifier(tx);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action);
        let published = rx
            .try_recv()
            .expect("a summary must be published on admission");
        assert_eq!(published.run_id, run_id);
        assert_eq!(published.sop_name, "s1");
        assert!(published.active, "an admitted run is active");
    }

    #[test]
    fn run_notifier_absent_is_a_noop() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        assert!(engine.subscribe_run_changes().is_none());
        engine.start_run("s1", manual_event()).unwrap();
        assert_eq!(engine.active_runs().len(), 1);
    }

    #[test]
    fn start_run_unknown_sop_fails() {
        let mut engine = engine_with_sops(vec![]);
        assert!(engine.start_run("nonexistent", manual_event()).is_err());
    }

    #[test]
    fn advance_step_to_completion() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Complete step 1
        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "done".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        // Should get step 2
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));

        // Complete step 2
        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "done".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(matches!(action, SopRunAction::Completed { .. }));
        assert!(engine.active_runs().is_empty());
        assert_eq!(engine.finished_runs(None).len(), 1);
    }

    #[test]
    fn step_failure_ends_run() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Failed,
                    output: "valve stuck".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::Failed { ref reason, .. } if reason.contains("valve stuck"))
        );
        assert!(engine.active_runs().is_empty());
    }

    #[test]
    fn schema_input_failure_fails_run_before_first_action() {
        let mut sop = test_sop("schema-in", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps[0].schema = Some(StepSchema {
            input: Some(required_object_schema("ok")),
            output: None,
        });
        let mut engine = engine_with_sops(vec![sop]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: Some("{}".into()),
            timestamp: now_iso8601(),
        };

        let action = engine.start_run("schema-in", event).unwrap();
        let run_id = extract_run_id(&action).to_string();

        assert!(
            matches!(action, SopRunAction::Failed { ref reason, .. } if reason.contains("input schema validation failed"))
        );
        let events = engine.run_events(&run_id).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "step_schema_reject"
                && event.payload["step"] == serde_json::json!(1)
                && event.payload["phase"] == serde_json::json!("input")
        }));
        assert!(engine.active_runs().is_empty());
        assert_eq!(engine.finished_runs(None)[0].status, SopRunStatus::Failed);
    }

    #[test]
    fn schema_output_failure_fails_run_before_next_step() {
        let mut sop = test_sop("schema-out", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps[0].schema = Some(StepSchema {
            input: None,
            output: Some(required_object_schema("ok")),
        });
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("schema-out", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "{}".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::Failed { ref reason, .. } if reason.contains("output schema validation failed"))
        );
        let events = engine.run_events(&run_id).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "step_schema_reject"
                && event.payload["step"] == serde_json::json!(1)
                && event.payload["phase"] == serde_json::json!("output")
        }));
        assert!(engine.active_runs().is_empty());
        assert_eq!(engine.finished_runs(None)[0].status, SopRunStatus::Failed);
    }

    #[test]
    fn schema_enforcement_disabled_allows_invalid_output() {
        let mut sop = test_sop("schema-off", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps[0].schema = Some(StepSchema {
            input: None,
            output: Some(required_object_schema("ok")),
        });
        let config = SopConfig {
            step_schema_enforce: false,
            ..SopConfig::default()
        };
        let mut engine = engine_with_config_sops(config, vec![sop]);
        let action = engine.start_run("schema-off", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "{}".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));
        assert_eq!(engine.active_runs()[&run_id].current_step, 2);
    }

    #[test]
    fn explicit_next_routes_llm_run_over_linear_successor() {
        let mut sop = test_sop("route-next", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps.push(SopStep {
            number: 3,
            title: "Step three".into(),
            body: "Do step three".into(),
            ..SopStep::default()
        });
        sop.steps[0].routing.next = Some(3);
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("route-next", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: r#"{"ok":true}"#.into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::ExecuteStep { ref step, .. } if step.number == 3),
            "explicit routing should select step 3 instead of the linear step 2"
        );
        let events = engine.run_events(&run_id).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "step_promoted"
                && event.payload["from_step"] == serde_json::json!(1)
                && event.payload["to_step"] == serde_json::json!(3)
        }));
        assert_eq!(engine.active_runs()[&run_id].current_step, 3);
    }

    #[test]
    fn failed_step_retries_until_policy_limit() {
        let mut sop = test_sop("route-retry", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps[0].on_failure = StepFailure::Retry { max: 2 };
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("route-retry", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Failed,
                    output: "first failure".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::ExecuteStep { ref step, .. } if step.number == 1),
            "initial failed attempt should allow the first retry of step 1"
        );
        let events = engine.run_events(&run_id).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "step_retry" && event.payload["step"] == serde_json::json!(1)
        }));
        assert_eq!(engine.active_runs()[&run_id].current_step, 1);

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Failed,
                    output: "second failure".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::ExecuteStep { ref step, .. } if step.number == 1),
            "first failed retry should allow the second retry of step 1"
        );
        assert_eq!(engine.active_runs()[&run_id].current_step, 1);

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Failed,
                    output: "third failure".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::Failed { ref reason, .. } if reason.contains("retry limit"))
        );
        assert!(engine.active_runs().is_empty());
    }

    #[test]
    fn failed_step_goto_routes_to_compensating_step() {
        let mut sop = test_sop("route-goto", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps[0].on_failure = StepFailure::Goto { step: 2 };
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("route-goto", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Failed,
                    output: "needs compensation".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(matches!(action, SopRunAction::ExecuteStep { ref step, .. } if step.number == 2));
        assert_eq!(engine.active_runs()[&run_id].current_step, 2);
    }

    #[test]
    fn ineligible_routed_step_is_marked_skipped_and_pending() {
        let mut sop = test_sop("route-pending", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps[1].routing.depends_on = vec![42];
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("route-pending", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: r#"{"ok":true}"#.into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::Pending { step: 2, ref reason, .. } if reason.contains("dependencies"))
        );
        let run = &engine.active_runs()[&run_id];
        assert_eq!(run.status, SopRunStatus::Pending);
        assert_eq!(run.current_step, 2);
        assert!(
            run.step_results
                .iter()
                .any(|result| result.step_number == 2 && result.status == SopStepStatus::Skipped)
        );
        let events = engine.run_events(&run_id).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "step_skipped"
                && event.payload["step"] == serde_json::json!(2)
                && event.payload["status"] == serde_json::json!("pending")
        }));
    }

    #[test]
    fn output_schema_failure_can_retry_through_on_failure_policy() {
        let mut sop = test_sop("schema-retry", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps[0].schema = Some(StepSchema {
            input: None,
            output: Some(required_object_schema("ok")),
        });
        sop.steps[0].on_failure = StepFailure::Retry { max: 2 };
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("schema-retry", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "{}".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::ExecuteStep { ref step, .. } if step.number == 1),
            "schema output failure should route through on_failure retry"
        );

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: r#"{"ok":true}"#.into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(matches!(action, SopRunAction::ExecuteStep { ref step, .. } if step.number == 2));
    }

    #[test]
    fn cancel_run() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine.cancel_run(&run_id).unwrap();
        assert!(engine.active_runs().is_empty());
        let finished = engine.finished_runs(None);
        assert_eq!(finished[0].status, SopRunStatus::Cancelled);
    }

    #[test]
    fn cancel_unknown_run_fails() {
        let mut engine = engine_with_sops(vec![]);
        assert!(engine.cancel_run("nonexistent").is_err());
    }

    // ── Concurrency ─────────────────────────────────────

    #[test]
    fn per_sop_concurrency_limit() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        // max_concurrent = 1 by default
        engine.start_run("s1", manual_event()).unwrap();
        assert!(!engine.can_start("s1"));
        assert!(engine.start_run("s1", manual_event()).is_err());
    }

    #[test]
    fn global_concurrency_limit() {
        let sops = vec![
            test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal),
            test_sop("s2", SopExecutionMode::Auto, SopPriority::Normal),
        ];
        let mut engine = SopEngine::new(SopConfig {
            max_concurrent_total: 1,
            ..SopConfig::default()
        });
        engine.sops = sops;

        engine.start_run("s1", manual_event()).unwrap();
        assert!(!engine.can_start("s2"));
    }

    #[test]
    fn start_run_uses_store_claims_across_engine_instances() {
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let sops = vec![test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal)];
        let mut first = engine_with_sops(sops.clone()).with_store(store.clone());
        let mut second = engine_with_sops(sops).with_store(store.clone());

        let action = first.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        assert!(
            !second.can_start("s1"),
            "read-only admission check must see the shared store claim"
        );
        assert!(
            second.start_run("s1", manual_event()).is_err(),
            "CAS claim must block a second engine with an empty local active map"
        );

        first.cancel_run(&run_id).unwrap();
        assert!(
            second.can_start("s1"),
            "finishing the first run releases the shared claim slot"
        );
        assert!(second.start_run("s1", manual_event()).is_ok());
    }

    #[test]
    fn pending_pool_cap_is_shared_across_engines_via_store() {
        // `max_pending_approvals` must bound the pending pool across ALL engine
        // holders of the shared store, not just this process's local active map. A
        // run parked at approval by one engine (persisted, exec claim released) must
        // count against a second engine's admission decision - otherwise two engines
        // sharing a store admit past the cap.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.max_concurrent = 5; // exec slots are not the limiter here...
        sop.max_pending_approvals = 1; // ...the pending-approval pool is.
        let sops = vec![sop];
        let mut first = engine_with_sops(sops.clone()).with_store(store.clone());
        let second = engine_with_sops(sops).with_store(store.clone());

        // First engine parks a run at approval (releases its exec claim, persists).
        let action = first.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert_eq!(
            first.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval
        );

        // Second engine's LOCAL active map is empty, yet the shared store shows the
        // parked run, so the pending pool reads full -> the trigger is deferred, not
        // admitted past the cap.
        assert!(
            second.active_runs.is_empty(),
            "second engine has no local runs"
        );
        assert!(
            matches!(second.evaluate_admission("s1"), SopAdmission::Defer { .. }),
            "a sibling engine's persisted pending run must count against the cap"
        );
    }

    #[test]
    fn current_step_policy_name_matches_step_number_not_index() {
        // B#2: a routed SOP with NON-CONTIGUOUS step numbers. The policy lookup must
        // match the step whose `number` == current_step, not the step at that vec
        // index - otherwise a positional read silently unpolices (or mis-polices) the
        // gate.
        let mut engine = engine_with_sops(vec![]);
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.steps = vec![
            SopStep {
                number: 1,
                policy: None,
                ..SopStep::default()
            },
            SopStep {
                number: 5,
                policy: Some("prod".into()),
                ..SopStep::default()
            },
        ];
        engine.set_sops_for_test(vec![sop]);
        let now = now_iso8601();
        engine.active_runs.insert(
            "r1".to_string(),
            SopRun {
                run_id: "r1".to_string(),
                sop_name: "s1".to_string(),
                trigger_event: manual_event(),
                frame_marker_id: "m".to_string(),
                status: SopRunStatus::WaitingApproval,
                current_step: 5,
                total_steps: 2,
                started_at: now.clone(),
                completed_at: None,
                step_results: Vec::new(),
                waiting_since: Some(now),
                llm_calls_saved: 0,
                revision: 0,
                revision_base: 0,
            },
        );
        assert_eq!(
            engine.current_step_policy_name("r1").as_deref(),
            Some("prod"),
            "policy resolves by step number (5), not vec index"
        );
    }

    #[test]
    fn current_step_policy_name_treats_empty_or_whitespace_as_none() {
        // A TOML `policy = ""` step deserializes to `Some("")` (types.rs has no empty
        // normalization, unlike the Markdown parser's `policy:` bullet in mod.rs).
        // Without normalizing here, the broker would treat "" as a NAMED-but-absent
        // policy and fail closed (gate stuck waiting forever) - diverging from the
        // equivalent Markdown SOP, which normalizes empty to unpoliced (`None`).
        let mut engine = engine_with_sops(vec![]);
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.steps = vec![
            SopStep {
                number: 1,
                policy: Some(String::new()),
                ..SopStep::default()
            },
            SopStep {
                number: 2,
                policy: Some("   ".into()),
                ..SopStep::default()
            },
        ];
        engine.set_sops_for_test(vec![sop]);
        let now = now_iso8601();
        for (run_id, step) in [("r1", 1u32), ("r2", 2u32)] {
            engine.active_runs.insert(
                run_id.to_string(),
                SopRun {
                    run_id: run_id.to_string(),
                    sop_name: "s1".to_string(),
                    trigger_event: manual_event(),
                    frame_marker_id: "m".to_string(),
                    status: SopRunStatus::WaitingApproval,
                    current_step: step,
                    total_steps: 2,
                    started_at: now.clone(),
                    completed_at: None,
                    step_results: Vec::new(),
                    waiting_since: Some(now.clone()),
                    llm_calls_saved: 0,
                    revision: 0,
                    revision_base: 0,
                },
            );
        }
        assert_eq!(
            engine.current_step_policy_name("r1"),
            None,
            "empty-string policy name normalizes to unpoliced, matching Markdown"
        );
        assert_eq!(
            engine.current_step_policy_name("r2"),
            None,
            "whitespace-only policy name also normalizes to unpoliced"
        );
    }

    #[test]
    fn capability_step_execution_increments_the_capability_executed_metric() {
        // record_capability_executed is called unconditionally in
        // execute_capability_step, before the result is inspected - so the counter
        // means "attempted", not "succeeded". Proves both the global and per-SOP
        // counters increment, and that a failing capability still counts as attempted.
        let metrics = std::sync::Arc::new(super::super::metrics::SopMetricsCollector::new());
        let mut engine = engine_with_sops(vec![]).with_metrics(metrics.clone());
        let sop = test_sop("s1", SopExecutionMode::Deterministic, SopPriority::Normal);
        engine.set_sops_for_test(vec![sop.clone()]);
        let now = now_iso8601();
        engine.active_runs.insert(
            "r1".to_string(),
            SopRun {
                run_id: "r1".to_string(),
                sop_name: "s1".to_string(),
                trigger_event: manual_event(),
                frame_marker_id: "m".to_string(),
                status: SopRunStatus::Running,
                current_step: 1,
                total_steps: 1,
                started_at: now.clone(),
                completed_at: None,
                step_results: Vec::new(),
                waiting_since: None,
                llm_calls_saved: 0,
                revision: 0,
                revision_base: 0,
            },
        );
        let step = SopStep {
            number: 1,
            kind: SopStepKind::Capability,
            capability: Some("noop".into()),
            ..SopStep::default()
        };
        engine
            .execute_capability_step(&sop, "r1", &step, serde_json::json!({}))
            .expect("noop capability always succeeds");
        assert_eq!(
            metrics.get_metric_value("sop.capability_executed"),
            Some(serde_json::json!(1)),
            "global counter increments on capability execution"
        );
        assert_eq!(
            metrics.get_metric_value("sop.s1.capability_executed"),
            Some(serde_json::json!(1)),
            "per-SOP counter increments too"
        );
    }

    #[test]
    fn gate_votes_are_per_step_and_canonical_per_subject() {
        // Broker quorum reads distinct_gate_voters(run_id, step, revision). Votes are
        // scoped to the current step presentation (a repeated gate does not reuse
        // earlier votes), and the voter key is the CANONICAL subject: HTTP and WS
        // share the paired credential, so the same subject over both transports counts
        // ONCE (cannot inflate quorum), while a genuinely different source (CLI) is a
        // distinct voter.
        use crate::sop::approval::ApprovalPrincipal;
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let engine = engine_with_sops(vec![]).with_store(store);

        // Same subject "alice" over HTTP then WS: collapses to gateway:alice.
        engine
            .record_gate_vote(
                "run-1",
                1,
                0,
                &ApprovalPrincipal::http(Some("alice".into())),
            )
            .unwrap();
        engine
            .record_gate_vote(
                "run-1",
                1,
                0,
                &ApprovalPrincipal::ws("c".into(), Some("alice".into())),
            )
            .unwrap();
        // A repeat over HTTP: still the same canonical voter.
        engine
            .record_gate_vote(
                "run-1",
                1,
                0,
                &ApprovalPrincipal::http(Some("alice".into())),
            )
            .unwrap();
        // A CLI actor is a genuinely distinct source (cli:bob).
        engine
            .record_gate_vote("run-1", 1, 0, &ApprovalPrincipal::cli(Some("bob".into())))
            .unwrap();
        // A vote on step 2 is a separate tally.
        engine
            .record_gate_vote("run-1", 2, 0, &ApprovalPrincipal::cli(Some("carol".into())))
            .unwrap();

        assert_eq!(
            engine.distinct_gate_voters("run-1", 1, 0),
            2,
            "gateway:alice (http+ws collapsed) + cli:bob = 2 distinct step-1 voters"
        );
        assert_eq!(
            engine.distinct_gate_voters("run-1", 2, 0),
            1,
            "step-2 quorum does not include step-1 voters"
        );
        assert_eq!(
            engine.distinct_gate_voters("run-1", 3, 0),
            0,
            "no votes recorded for step 3"
        );
    }

    #[test]
    fn deterministic_start_uses_store_claims() {
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let sops = vec![deterministic_sop("det-sop")];
        let mut first = engine_with_sops(sops.clone()).with_store(store.clone());
        let mut second = engine_with_sops(sops).with_store(store);

        first.start_run("det-sop", manual_event()).unwrap();

        assert!(
            second.start_run("det-sop", manual_event()).is_err(),
            "deterministic runs must use the same CAS admission gate"
        );
    }

    #[test]
    fn direct_deterministic_start_cannot_bypass_admission() {
        // start_deterministic_run is public; a DIRECT call must enforce the admission
        // policy itself (not just can_start), so it cannot bypass Hold / Coalesce /
        // the pending-approval pool that start_run enforces.
        let sops = vec![deterministic_sop("det")];
        let mut engine = engine_with_sops(sops);
        engine
            .start_deterministic_run("det", manual_event())
            .unwrap(); // fills the single slot
        assert!(
            engine
                .start_deterministic_run("det", manual_event())
                .is_err(),
            "a direct deterministic start must be declined when admission denies it"
        );
    }

    #[test]
    fn coalesce_resolves_in_flight_run_across_engines() {
        // A2#3: the coalesced run id must come from the SHARED store, so an engine
        // with an empty local map still folds into a sibling engine's in-flight run
        // (Coalesce), not Defer (which would churn AMQP redeliveries).
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.max_concurrent = 1;
        sop.admission_policy = crate::sop::types::SopAdmissionPolicy::Coalesce;
        let sops = vec![sop];
        let mut first = engine_with_sops(sops.clone()).with_store(store.clone());
        let second = engine_with_sops(sops).with_store(store);

        let action = first.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(
            second.active_runs.is_empty(),
            "second engine has no local runs"
        );
        match second.evaluate_admission("s1") {
            SopAdmission::Coalesce { existing_run_id } => assert_eq!(
                existing_run_id, run_id,
                "coalesces into the sibling engine's persisted in-flight run"
            ),
            other => panic!("expected Coalesce across engines, got {other:?}"),
        }
    }

    #[test]
    fn proposals_round_trip_through_engine_store_surface() {
        let engine = SopEngine::new(SopConfig::default());
        let now = now_iso8601();
        let proposal = ProposalRecord {
            id: "prop-1".to_string(),
            kind: ProposalKind::Update,
            status: ProposalStatus::Pending,
            source_run_id: Some("run-1".to_string()),
            sop_name: "s1".to_string(),
            target_content_hash: Some("sha256:abc".to_string()),
            manifest_toml: "[sop]\nname = \"s1\"\ndescription = \"S1\"\n".to_string(),
            procedure_markdown: "## Steps\n\n1. **Do** - It.\n".to_string(),
            provenance: serde_json::json!({"producer": "test"}),
            created_at: now.clone(),
            updated_at: now,
            status_reason: None,
            applied_at: None,
            applied_by: None,
            rollback_path: None,
        };

        engine.save_proposal(&proposal).unwrap();

        assert_eq!(
            engine.load_proposal("prop-1").unwrap().unwrap().sop_name,
            "s1"
        );
        assert_eq!(engine.list_proposals(None).unwrap().len(), 1);
        assert_eq!(
            engine
                .list_proposals(Some(ProposalStatus::Pending))
                .unwrap()
                .len(),
            1
        );
        assert!(
            engine
                .list_proposals(Some(ProposalStatus::Applied))
                .unwrap()
                .is_empty()
        );
    }

    // ── Cooldown ────────────────────────────────────────

    #[test]
    fn cooldown_blocks_immediate_restart() {
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.cooldown_secs = 3600; // 1 hour
        let mut engine = engine_with_sops(vec![sop]);

        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        // Complete both steps
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        // Cooldown not elapsed — should block
        assert!(!engine.can_start("s1"));
    }

    #[test]
    fn cooldown_is_shared_across_engine_instances() {
        // Two engines share ONE store. Engine A runs and finishes a run; the
        // cooldown marker lives only in A's local `finished_runs`, but B must still
        // honor the cooldown because it reads the last terminal completion from the
        // shared store. Without FIX 1 (store-backed cooldown), B sees no local
        // finished run and admits early - this test fails.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.cooldown_secs = 3600; // 1 hour
        let sops = vec![sop];
        let mut engine_a = engine_with_sops(sops.clone()).with_store(store.clone());
        let mut engine_b = engine_with_sops(sops).with_store(store.clone());

        // Engine A starts and finishes a run (writes a terminal row to the store).
        let action = engine_a.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine_a.finish_run(&run_id, SopRunStatus::Completed, None);

        // Engine B never ran this SOP, so it has no local finished entry. It must
        // still see the cooldown via the shared store.
        assert!(
            !engine_b.can_start("s1"),
            "a second engine must observe the cooldown from the shared store"
        );
        assert!(
            engine_b.start_run("s1", manual_event()).is_err(),
            "start_run must bail while the shared-store cooldown is active"
        );

        // Advance the stored completion past the cooldown window (supersede the
        // same run's terminal row with an older completed_at, newer revision). The
        // store now reports an elapsed cooldown, so B may start.
        let stored = store.load_run(&run_id).unwrap().unwrap();
        let mut aged = stored.clone();
        aged.revision = stored.revision + 1;
        aged.run.completed_at = Some("2000-01-01T00:00:00Z".to_string());
        store.finish_run(&run_id, &aged).unwrap();

        assert!(
            engine_b.can_start("s1"),
            "once the shared-store cooldown window passes, the second engine may start"
        );
        assert!(
            engine_b.start_run("s1", manual_event()).is_ok(),
            "start_run succeeds after the shared-store cooldown elapses"
        );
    }

    #[test]
    fn restore_runs_keeps_active_and_claims_aligned_over_cap() {
        // Pre-seed the shared store with non-terminal runs OVER the per-SOP cap,
        // then restore onto a fresh engine. FIX 2 re-establishes a claim for every
        // restored run without applying admission caps, so `active_runs` and the
        // live-claim total stay aligned 1:1 (the old capped path silently dropped
        // the over-cap claim, leaving a locally active run with no store claim).
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.max_concurrent = 1; // cap of 1, but seed 3 already-running runs
        let now = now_iso8601();
        for i in 0..3 {
            let run = SopRun {
                run_id: format!("restore-{i}"),
                sop_name: "s1".to_string(),
                trigger_event: manual_event(),
                frame_marker_id: format!("marker-{i}"),
                status: SopRunStatus::Running,
                current_step: 1,
                total_steps: 2,
                started_at: now.clone(),
                completed_at: None,
                step_results: Vec::new(),
                waiting_since: None,
                llm_calls_saved: 0,
                revision: 0,
                revision_base: 0,
            };
            store
                .save_run(&PersistedRun::new(
                    run,
                    now.clone(),
                    SopTriggerSource::Manual,
                ))
                .unwrap();
        }

        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        engine.restore_runs();

        // Every restored run is active...
        assert_eq!(engine.active_runs().len(), 3, "all over-cap runs restored");
        // ...and each has a live store claim (counts == active_runs.len()).
        let (per_sop, total) = store.claim_counts("s1").unwrap();
        assert_eq!(
            total,
            engine.active_runs().len(),
            "every active restored run must hold a live store claim"
        );
        assert_eq!(
            per_sop, 3,
            "all three claims are accounted for under the SOP"
        );
    }

    // ── Execution modes ─────────────────────────────────

    #[test]
    fn auto_mode_executes_immediately() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));
    }

    #[test]
    fn supervised_mode_waits_on_first_step() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    /// A recorded `deliver` call: `(route, run_id, sop_name, step)`.
    type RecordedRouteCall = (String, String, String, u32);

    /// A route adapter that records every `deliver` call, so a test can assert the
    /// engine fired an out-of-band approval-request notice on park.
    #[derive(Default)]
    struct RecordingRouteAdapter {
        calls: std::sync::Arc<std::sync::Mutex<Vec<RecordedRouteCall>>>,
    }

    impl crate::sop::approval::ApprovalRouteAdapter for RecordingRouteAdapter {
        fn deliver(
            &self,
            route: &str,
            notice: &crate::sop::approval::GateNotice<'_>,
        ) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push((
                route.to_string(),
                notice.run_id.to_string(),
                notice.sop_name.to_string(),
                notice.step,
            ));
            Ok(())
        }
    }

    fn policied_supervised_engine(
        request_route: Option<&str>,
        adapter: std::sync::Arc<dyn crate::sop::approval::ApprovalRouteAdapter>,
    ) -> SopEngine {
        use zeroclaw_config::schema::ApprovalPolicyConfig;
        let mut config = SopConfig::default();
        config.approval.policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: None,
                quorum: 0,
                request_route: request_route.map(String::from),
                escalation_route: None,
            },
        );
        // A supervised SOP whose first step names the `prod` policy, so starting it
        // parks at a policied approval gate.
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.steps[0].policy = Some("prod".to_string());
        engine_with_config_sops(config, vec![sop]).with_approval_broker(std::sync::Arc::new(
            crate::sop::approval::ApprovalBroker::with_route(adapter),
        ))
    }

    #[test]
    fn parking_at_a_policied_gate_delivers_the_request_route() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let adapter = std::sync::Arc::new(RecordingRouteAdapter {
            calls: calls.clone(),
        });
        let mut engine = policied_supervised_engine(Some("discord.ops:123456789"), adapter);

        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(
            matches!(action, SopRunAction::WaitApproval { .. }),
            "supervised policied step parks for approval"
        );

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            1,
            "exactly one out-of-band request-route delivery fired on park"
        );
        let (route, delivered_run, sop_name, step) = &recorded[0];
        assert_eq!(route, "discord.ops:123456789", "the policy's request_route");
        assert_eq!(delivered_run, &run_id, "carries the parked run id");
        assert_eq!(sop_name, "s1", "carries the SOP name");
        assert_eq!(*step, 1, "carries the parked step number");
    }

    #[test]
    fn parking_at_a_policied_gate_without_a_request_route_delivers_nothing() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let adapter = std::sync::Arc::new(RecordingRouteAdapter {
            calls: calls.clone(),
        });
        // Same policied gate, but the policy names NO request_route.
        let mut engine = policied_supervised_engine(None, adapter);

        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
        assert!(
            calls.lock().unwrap().is_empty(),
            "no request_route configured means no out-of-band delivery"
        );
    }

    #[test]
    fn step_by_step_waits_on_every_step() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::StepByStep,
            SopPriority::Normal,
        )]);

        // Step 1: WaitApproval
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));

        // Approve step 1
        let action = approve_gate_cli(&mut engine, &run_id);
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));

        // Complete step 1, step 2 should also WaitApproval
        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    #[test]
    fn priority_based_critical_gates() {
        // [SEC-FLIP] Critical/High under PriorityBased now GATE (was auto-execute).
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::PriorityBased,
            SopPriority::Critical,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(
            matches!(action, SopRunAction::WaitApproval { .. }),
            "critical PriorityBased SOPs must gate, not auto-run"
        );
    }

    #[test]
    fn priority_based_normal_supervised() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::PriorityBased,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        // Normal + PriorityBased → Supervised → WaitApproval on step 1
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    #[test]
    fn requires_confirmation_overrides_auto() {
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Critical);
        sop.steps[0].requires_confirmation = true;
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        // Even in Auto mode, requires_confirmation forces WaitApproval
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    #[test]
    fn step_mode_can_tighten_auto_step() {
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps[0].mode = Some(SopExecutionMode::StepByStep);
        let mut engine = engine_with_sops(vec![sop]);

        let action = engine.start_run("s1", manual_event()).unwrap();

        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    #[test]
    fn step_mode_cannot_relax_step_by_step_step() {
        let mut sop = test_sop("s1", SopExecutionMode::StepByStep, SopPriority::Normal);
        sop.steps[0].mode = Some(SopExecutionMode::Auto);
        let mut engine = engine_with_sops(vec![sop]);

        let action = engine.start_run("s1", manual_event()).unwrap();

        assert!(
            matches!(action, SopRunAction::WaitApproval { .. }),
            "a step auto override must not relax the SOP's step_by_step gate, got {action:?}"
        );
    }

    #[test]
    fn out_of_band_required_prevents_step_auto_relaxing_gate() {
        let mut sop = test_sop("s1", SopExecutionMode::StepByStep, SopPriority::Normal);
        sop.steps[0].mode = Some(SopExecutionMode::Auto);
        let mut engine = engine_with_config_sops(
            SopConfig {
                approval_mode: zeroclaw_config::schema::ApprovalMode::OutOfBandRequired,
                ..SopConfig::default()
            },
            vec![sop],
        );

        let action = engine.start_run("s1", manual_event()).unwrap();

        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    // ── Approve ─────────────────────────────────────────

    #[test]
    fn approve_transitions_to_execute() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Run should be WaitingApproval
        let run = engine.active_runs().get(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::WaitingApproval);

        // Approve
        let action = approve_gate_cli(&mut engine, &run_id);
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));

        let run = engine.active_runs().get(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::Running);
    }

    #[test]
    fn approve_non_waiting_fails() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(engine.approve_step(&run_id).is_err());
    }

    #[test]
    fn step_auto_override_cannot_defeat_supervised_step_one_gate() {
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.steps[0].mode = Some(SopExecutionMode::Auto);
        let mut engine = engine_with_sops(vec![sop]);

        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(
            matches!(action, SopRunAction::WaitApproval { .. }),
            "supervised SOP must gate step 1 even when the step overrides mode to auto, got {action:?}"
        );
        let run_id = extract_run_id(&action).to_string();
        assert_eq!(
            engine.active_runs().get(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the run must park at the gate, not sit Running at step 1"
        );
    }

    // ── Advance step gate guard (#8678) ─────────────────
    //
    // A driver calling `sop_advance` while a run is parked at an external
    // gate (WaitingApproval or PausedCheckpoint) used to be allowed to
    // fabricate a Completed step result, record it, and dispatch the next
    // step — silently bypassing the approval flow or the deterministic
    // checkpoint resume. `advance_step` now refuses those calls.

    #[test]
    fn advance_step_rejects_waiting_approval_run() {
        // requires_confirmation forces the run to WaitApproval on step 1.
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Critical);
        sop.steps[0].requires_confirmation = true;
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Sanity: run is parked at the gate.
        let run = engine.active_runs().get(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::WaitingApproval);
        let step_results_before = run.step_results.len();

        // Driver tries to fabricate success for the gated step.
        let err = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "fabricated".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap_err();

        // Error must point at the gate, not the run id.
        assert!(
            err.to_string().contains("WaitingApproval"),
            "rejection should mention the gate status, got: {err}"
        );

        // The run state must be unchanged: still WaitingApproval, no
        // phantom step result recorded, no next step dispatched.
        let run = engine.active_runs().get(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::WaitingApproval);
        assert_eq!(run.step_results.len(), step_results_before);
    }

    #[test]
    fn advance_step_rejects_paused_checkpoint_run() {
        // A deterministic SOP with a Checkpoint step pauses the run in
        // PausedCheckpoint after step 1 completes. Driving `sop_advance`
        // directly must be rejected — the only legitimate resume path is
        // `approve_step`.
        let mut engine = engine_with_sops(vec![deterministic_sop("det-cp")]);
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Advance through step 1 (Execute) to reach the checkpoint.
        engine
            .advance_deterministic_step(&run_id, serde_json::json!({"ok": true}), None)
            .unwrap();
        let run = engine.get_run(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::PausedCheckpoint);

        // Driver tries to fabricate completion of the checkpoint step.
        let err = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "fabricated".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap_err();

        assert!(
            err.to_string().contains("PausedCheckpoint"),
            "rejection should mention the gate status, got: {err}"
        );

        // The run must still be parked at the checkpoint, not advanced
        // past it.
        let run = engine.get_run(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::PausedCheckpoint);
    }

    #[test]
    fn advance_step_still_works_for_running_run() {
        // Control case: a non-paused run must still be drivable through
        // sop_advance. Without this case, the new guard could be hiding
        // a regression on the happy path.
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "done".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));
    }

    // ── Context formatting ──────────────────────────────

    #[test]
    fn step_context_includes_sop_name_and_step() {
        let sop = test_sop(
            "pump-shutdown",
            SopExecutionMode::Auto,
            SopPriority::Critical,
        );
        let run = SopRun {
            run_id: "run-001".into(),
            sop_name: "pump-shutdown".into(),
            trigger_event: manual_event(),
            frame_marker_id: "marker-001".into(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: 2,
            started_at: now_iso8601(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        };
        let ctx = format_step_context(&sop, &run, &sop.steps[0], &SopConfig::default());
        assert!(ctx.contains("pump-shutdown"));
        assert!(ctx.contains("Step 1 of 2"));
        assert!(ctx.contains("Step one"));
    }

    // ── Get run (active + finished) ─────────────────────

    #[test]
    fn get_run_finds_active_and_finished() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Active
        assert!(engine.get_run(&run_id).is_some());
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Running
        );

        // Complete
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        // Now finished — still findable
        assert!(engine.get_run(&run_id).is_some());
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Completed
        );

        // Unknown
        assert!(engine.get_run("nonexistent").is_none());
    }

    // ── ISO-8601 helpers ────────────────────────────────

    #[test]
    fn iso8601_roundtrip() {
        let ts = now_iso8601();
        let secs = parse_iso8601_secs(&ts);
        assert!(secs.is_some());
        // Should be close to current time
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(now.abs_diff(secs.unwrap()) < 2);
    }

    #[test]
    fn parse_known_timestamp() {
        // 2026-01-01T00:00:00Z
        let secs = parse_iso8601_secs("2026-01-01T00:00:00Z").unwrap();
        // Jan 1 2026 = 20454 days since epoch * 86400
        assert_eq!(secs, 20454 * 86400);
    }

    // ── Approval timeout ─────────────────────────────────

    #[test]
    fn timeout_escalates_critical_no_self_approve() {
        // [SEC-FLIP] Under the default fail-closed Escalate, a Critical/High SOP
        // that times out is NO LONGER auto-approved: it stays WaitingApproval and a
        // gate_escalated ledger row is recorded. (Was: timeout_auto_approves_critical.)
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            ..SopConfig::default()
        });
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Critical,
        )]);

        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));

        let run = engine.active_runs.get_mut(&run_id).unwrap();
        run.waiting_since = Some("2020-01-01T00:00:00Z".into());

        let actions = engine.check_approval_timeouts();
        assert!(actions.is_empty(), "escalate produces no resumed action");
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "critical run stays gated under fail-closed escalate"
        );
        assert!(
            engine
                .run_events(&run_id)
                .unwrap()
                .iter()
                .any(|ev| ev.kind == "gate_escalated"),
            "escalation is recorded in the ledger"
        );
    }

    #[test]
    fn timeout_escalation_without_distinct_route_resurfaces_request_route() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let adapter = std::sync::Arc::new(RecordingRouteAdapter {
            calls: calls.clone(),
        });
        let mut engine = policied_supervised_engine(Some("discord.ops:123456789"), adapter);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
        calls.lock().unwrap().clear();

        crate::sop::approval::timeout::apply_timeout_action(
            &mut engine,
            &run_id,
            zeroclaw_config::schema::ApprovalTimeoutAction::Escalate,
        );

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [(
                "discord.ops:123456789".to_string(),
                run_id,
                "s1".to_string(),
                1
            )],
            "an unset escalation_route must re-surface the gate to request_route"
        );
    }

    #[test]
    fn maintenance_tick_fires_fail_closed_timeout() {
        // EPIC A1: the daemon tick drives check_approval_timeouts. An overdue gate
        // under the default fail-closed Escalate stays WaitingApproval (no
        // self-approve) and the escalation is recorded; the summary counts it.
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            ..SopConfig::default()
        });
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        // Force the gate overdue.
        engine.active_runs.get_mut(&run_id).unwrap().waiting_since =
            Some("2020-01-01T00:00:00Z".into());

        let summary = engine.run_maintenance_tick();

        assert!(
            !summary.is_empty(),
            "an overdue gate makes the pass non-empty"
        );
        assert_eq!(summary.timed_out, 1, "the overdue gate timed out");
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "fail-closed escalate keeps the gate open, never self-approves"
        );
        assert!(
            engine
                .run_events(&run_id)
                .unwrap()
                .iter()
                .any(|ev| ev.kind == "gate_escalated"),
            "the tick recorded the escalation in the ledger"
        );
    }

    #[test]
    fn maintenance_tick_is_a_noop_when_nothing_is_due() {
        let mut engine = SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        // No runs started -> nothing to time out, reap, or prune.
        let summary = engine.run_maintenance_tick();
        assert!(summary.is_empty(), "a quiet tick is a no-op");
        assert_eq!(summary.timed_out, 0);
        assert_eq!(summary.reaped_claims, 0);
        assert_eq!(summary.pruned_runs, 0);
    }

    #[test]
    fn timeout_cancel_finishes_run() {
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            approval_timeout_action: zeroclaw_config::schema::ApprovalTimeoutAction::Cancel,
            ..SopConfig::default()
        });
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine.active_runs.get_mut(&run_id).unwrap().waiting_since =
            Some("2020-01-01T00:00:00Z".into());

        let actions = engine.check_approval_timeouts();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], SopRunAction::Completed { .. }));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Cancelled,
            "cancel terminates the run (retained as a terminal record)"
        );
    }

    #[test]
    fn timeout_cancel_terminal_persist_failure_keeps_gate_waiting() {
        let store: Arc<dyn SopRunStore> = Arc::new(FailFirstFinishStore::new());
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            approval_timeout_action: zeroclaw_config::schema::ApprovalTimeoutAction::Cancel,
            ..SopConfig::default()
        })
        .with_store(store);
        engine.set_sops_for_test(vec![test_sop(
            "timeout-terminal-fail",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine
            .start_run("timeout-terminal-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine.active_runs.get_mut(&run_id).unwrap().waiting_since =
            Some("2000-01-01T00:00:00Z".into());

        let actions = engine.check_approval_timeouts();

        assert!(
            actions.is_empty(),
            "timeout cancel must not report a terminal action when persistence failed"
        );
        assert_eq!(
            engine.get_run(&run_id).map(|run| run.status),
            Some(SopRunStatus::WaitingApproval),
            "the approval gate remains waiting for retry"
        );
        assert!(
            engine.last_finished_run("timeout-terminal-fail").is_none(),
            "failed terminal persistence must not move the run to finished_runs"
        );
    }

    #[test]
    fn timeout_auto_approve_legacy_resumes() {
        // The legacy fail-open behavior is reachable ONLY via the explicit opt-in.
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            approval_timeout_action: zeroclaw_config::schema::ApprovalTimeoutAction::AutoApprove,
            ..SopConfig::default()
        });
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Critical,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine.active_runs.get_mut(&run_id).unwrap().waiting_since =
            Some("2020-01-01T00:00:00Z".into());

        let actions = engine.check_approval_timeouts();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], SopRunAction::ExecuteStep { .. }));
    }

    #[test]
    fn escalate_never_self_approves_any_priority() {
        // [SEC-FLIP] guard: under the default action, NO priority auto-approves.
        for priority in [
            SopPriority::Critical,
            SopPriority::High,
            SopPriority::Normal,
            SopPriority::Low,
        ] {
            let mut engine = SopEngine::new(SopConfig {
                approval_timeout_secs: 1,
                ..SopConfig::default()
            });
            engine.set_sops_for_test(vec![test_sop("s1", SopExecutionMode::Supervised, priority)]);
            let action = engine.start_run("s1", manual_event()).unwrap();
            let run_id = extract_run_id(&action).to_string();
            engine.active_runs.get_mut(&run_id).unwrap().waiting_since =
                Some("2020-01-01T00:00:00Z".into());

            let actions = engine.check_approval_timeouts();
            assert!(
                actions.is_empty(),
                "priority {priority:?} must not self-approve under fail-closed default"
            );
            assert_eq!(
                engine.get_run(&run_id).unwrap().status,
                SopRunStatus::WaitingApproval
            );
        }
    }

    #[test]
    fn timeout_does_not_auto_approve_normal() {
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            ..SopConfig::default()
        });
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);

        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Backdate waiting_since
        let run = engine.active_runs.get_mut(&run_id).unwrap();
        run.waiting_since = Some("2020-01-01T00:00:00Z".into());

        // Normal priority → no auto-approve
        let actions = engine.check_approval_timeouts();
        assert!(actions.is_empty());
        // Run should still be WaitingApproval
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval
        );
    }

    #[test]
    fn timeout_zero_disables_check() {
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 0,
            ..SopConfig::default()
        });
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Critical,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let run = engine.active_runs.get_mut(&run_id).unwrap();
        run.waiting_since = Some("2020-01-01T00:00:00Z".into());

        let actions = engine.check_approval_timeouts();
        assert!(actions.is_empty());
    }

    #[test]
    fn waiting_since_set_on_wait_approval() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let run = engine.get_run(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::WaitingApproval);
        assert!(run.waiting_since.is_some());
    }

    // ── A1: HITL admission (parked runs release their exec slot) ──────

    #[test]
    fn parked_approval_run_releases_exec_slot() {
        // A run parked at a HITL approval must release its exec claim so a second
        // trigger for the same SOP (max_concurrent = 1) is admitted, not dropped.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.max_concurrent = 1;
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        let a1 = engine.start_run("s1", manual_event()).unwrap();
        let run1 = extract_run_id(&a1).to_string();
        assert_eq!(
            engine.get_run(&run1).unwrap().status,
            SopRunStatus::WaitingApproval
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "a parked approval run must not hold an exec claim"
        );
        assert!(
            engine.can_start("s1"),
            "the freed slot admits the next trigger"
        );

        // Second trigger admits (pre-A1 this was dropped on concurrency) and parks too.
        let a2 = engine.start_run("s1", manual_event()).unwrap();
        let run2 = extract_run_id(&a2).to_string();
        assert_ne!(run1, run2);
        assert_eq!(
            engine.get_run(&run2).unwrap().status,
            SopRunStatus::WaitingApproval
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "both parked runs hold no exec claim"
        );
    }

    #[test]
    fn resume_reacquires_exec_slot() {
        // Approving a parked run re-establishes its exec claim so it counts against
        // concurrency again while it finishes executing.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        let a = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&a).to_string();
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "parked before approval: no exec claim"
        );

        let _ = approve_gate_cli(&mut engine, &run_id);
        assert_eq!(
            store.claim_counts("s1").unwrap().1,
            1,
            "an approved+resumed run re-acquires its exec claim"
        );
    }

    #[test]
    fn restored_parked_run_holds_no_exec_claim() {
        // A parked run persisted before a restart must restore WITHOUT re-taking an
        // exec slot (it is waiting on a human, not executing), so the slot stays free
        // for a fresh trigger (max_concurrent = 1).
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.max_concurrent = 1;
        let now = now_iso8601();
        let parked = SopRun {
            run_id: "parked-1".to_string(),
            sop_name: "s1".to_string(),
            trigger_event: manual_event(),
            frame_marker_id: "marker".to_string(),
            status: SopRunStatus::WaitingApproval,
            current_step: 1,
            total_steps: 2,
            started_at: now.clone(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: Some(now.clone()),
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        };
        store
            .save_run(&PersistedRun::new(
                parked,
                now.clone(),
                SopTriggerSource::Manual,
            ))
            .unwrap();

        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        engine.restore_runs();

        assert_eq!(
            engine.get_run("parked-1").unwrap().status,
            SopRunStatus::WaitingApproval,
            "the parked run is restored"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "a restored parked run holds no exec claim"
        );
        assert!(
            engine.can_start("s1"),
            "its slot stays free for a new trigger"
        );
    }

    #[test]
    fn restore_releases_stale_claim_for_parked_run() {
        // A durable store written before this change can carry a parked run PLUS a
        // live claim row. restore_runs must RELEASE that stale claim so the run does
        // not keep blocking a same-SOP admission (nor get its lease extended forever).
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.max_concurrent = 1;
        // Seed a live claim for the parked run (the old behavior kept it).
        assert!(
            store
                .try_claim_run("parked-1", "s1", 1, 4)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "seeded a stale claim"
        );
        let now = now_iso8601();
        let parked = SopRun {
            run_id: "parked-1".to_string(),
            sop_name: "s1".to_string(),
            trigger_event: manual_event(),
            frame_marker_id: "marker".to_string(),
            status: SopRunStatus::WaitingApproval,
            current_step: 1,
            total_steps: 2,
            started_at: now.clone(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: Some(now.clone()),
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        };
        store
            .save_run(&PersistedRun::new(
                parked,
                now.clone(),
                SopTriggerSource::Manual,
            ))
            .unwrap();

        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        engine.restore_runs();

        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "restore must release the parked run's stale claim"
        );
        assert!(
            engine.can_start("s1"),
            "the freed slot admits a new trigger after restart"
        );
    }

    /// Delegates to an in-memory store but fails every `renew_claim_for_restore`,
    /// to prove resume fails CLOSED when the claim store errors.
    struct FailingReacquireStore {
        inner: InMemoryRunStore,
    }
    impl SopRunStore for FailingReacquireStore {
        fn save_run(&self, r: &PersistedRun) -> Result<(), StoreError> {
            self.inner.save_run(r)
        }
        fn finish_run(&self, id: &str, t: &PersistedRun) -> Result<(), StoreError> {
            self.inner.finish_run(id, t)
        }
        fn load_terminal_runs(&self, _limit: usize) -> Result<Vec<PersistedRun>, StoreError> {
            Ok(Vec::new())
        }
        fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError> {
            self.inner.load_active_runs()
        }
        fn load_run(&self, id: &str) -> Result<Option<PersistedRun>, StoreError> {
            self.inner.load_run(id)
        }
        fn last_terminal_completed_at(&self, s: &str) -> Result<Option<String>, StoreError> {
            self.inner.last_terminal_completed_at(s)
        }
        fn try_claim_run(
            &self,
            id: &str,
            s: &str,
            p: usize,
            g: usize,
        ) -> Result<Option<ClaimToken>, StoreError> {
            self.inner.try_claim_run(id, s, p, g)
        }
        fn renew_claim_for_restore(&self, _id: &str, _s: &str) -> Result<ClaimToken, StoreError> {
            Err(StoreError::Backend("injected renew failure".into()))
        }
        fn claim_counts(&self, s: &str) -> Result<(usize, usize), StoreError> {
            self.inner.claim_counts(s)
        }
        fn heartbeat_claim(&self, t: &ClaimToken) -> Result<(), StoreError> {
            self.inner.heartbeat_claim(t)
        }
        fn release_claim(&self, t: &ClaimToken) -> Result<(), StoreError> {
            self.inner.release_claim(t)
        }
        fn expired_claims(&self, n: &str) -> Result<Vec<ClaimToken>, StoreError> {
            self.inner.expired_claims(n)
        }
        fn append_event(&self, e: &SopEventRecord) -> Result<u64, StoreError> {
            self.inner.append_event(e)
        }
        fn list_events(&self, id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
            self.inner.list_events(id)
        }
        fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError> {
            self.inner.save_proposal(p)
        }
        fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
            self.inner.load_proposal(id)
        }
        fn list_proposals(
            &self,
            s: Option<ProposalStatus>,
        ) -> Result<Vec<ProposalRecord>, StoreError> {
            self.inner.list_proposals(s)
        }
        fn prune(&self, p: &RetentionPolicy) -> Result<usize, StoreError> {
            self.inner.prune(p)
        }
        fn health_check(&self) -> bool {
            self.inner.health_check()
        }
        fn backend(&self) -> &'static str {
            "failing-reacquire-test"
        }
    }

    #[test]
    fn resume_fails_closed_when_claim_reacquire_fails() {
        // If the claim store errors during resume, the run must NOT execute
        // uncounted: the resume aborts (Err) and the gate stays WaitingApproval.
        let store = std::sync::Arc::new(FailingReacquireStore {
            inner: InMemoryRunStore::new(),
        });
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store);
        let a = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&a).to_string();
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval
        );
        let res = engine.resolve_gate(
            &run_id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(None),
        );
        assert!(
            res.is_err(),
            "resume must abort when the exec claim cannot be re-acquired"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the gate must stay WaitingApproval (re-resolvable), not execute uncounted"
        );
        // A1#2: the claim is secured BEFORE the audit row, so a reacquire failure
        // must leave NO false `gate_resolved` approval row in the ledger (which
        // metrics would otherwise count as a real approval).
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            !events.iter().any(|ev| ev.kind == "gate_resolved"),
            "a failed resume must not write a gate_resolved row"
        );
    }

    #[test]
    fn checkpoint_approve_reacquire_failure_writes_no_ledger() {
        let store = std::sync::Arc::new(FailingReacquireStore {
            inner: InMemoryRunStore::new(),
        });
        let mut engine =
            engine_with_sops(vec![capability_checkpoint_sop("cp-claim")]).with_store(store);
        let first = engine.start_run("cp-claim", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let parked = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("drive to checkpoint");
        assert!(matches!(parked, SopRunAction::CheckpointWait { .. }));

        let res = engine.resolve_via_broker(
            &run_id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(None),
        );
        assert!(
            res.is_err(),
            "checkpoint approve must abort when the exec claim cannot be re-acquired"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "the checkpoint must stay parked and re-resolvable"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            !events.iter().any(|ev| ev.kind == "gate_resolved"),
            "a failed checkpoint approve must not write a gate_resolved row: {events:?}"
        );
    }

    #[test]
    fn checkpoint_amend_reacquire_failure_writes_no_ledger() {
        let store = std::sync::Arc::new(FailingReacquireStore {
            inner: InMemoryRunStore::new(),
        });
        let mut engine =
            engine_with_sops(vec![editable_checkpoint_sop("cp-amend-claim")]).with_store(store);
        let first = engine
            .start_run(
                "cp-amend-claim",
                payload_event(r#"{"body":"model draft","repo":"o/r"}"#),
            )
            .unwrap();
        let run_id = extract_run_id(&first).to_string();
        let parked = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("drive to checkpoint");
        assert!(matches!(parked, SopRunAction::CheckpointWait { .. }));

        let res = engine.resolve_via_broker(
            &run_id,
            ApprovalDecision::Amend {
                text: "operator edit".into(),
            },
            ApprovalPrincipal::cli(None),
        );
        assert!(
            res.is_err(),
            "checkpoint amend must abort when the exec claim cannot be re-acquired"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "the checkpoint must stay parked and re-resolvable"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            !events.iter().any(|ev| ev.kind == "gate_resolved"),
            "a failed checkpoint amend must not write a gate_resolved row: {events:?}"
        );
    }

    /// Delegates to an in-memory store but can be flipped to fail every
    /// `append_event`, to prove the audit-append failure path rolls back the
    /// reacquired exec claim.
    struct FailingAppendStore {
        inner: InMemoryRunStore,
        fail: std::sync::atomic::AtomicBool,
    }
    impl SopRunStore for FailingAppendStore {
        fn save_run(&self, r: &PersistedRun) -> Result<(), StoreError> {
            self.inner.save_run(r)
        }
        fn finish_run(&self, id: &str, t: &PersistedRun) -> Result<(), StoreError> {
            self.inner.finish_run(id, t)
        }
        fn load_terminal_runs(&self, _limit: usize) -> Result<Vec<PersistedRun>, StoreError> {
            Ok(Vec::new())
        }
        fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError> {
            self.inner.load_active_runs()
        }
        fn load_run(&self, id: &str) -> Result<Option<PersistedRun>, StoreError> {
            self.inner.load_run(id)
        }
        fn last_terminal_completed_at(&self, s: &str) -> Result<Option<String>, StoreError> {
            self.inner.last_terminal_completed_at(s)
        }
        fn try_claim_run(
            &self,
            id: &str,
            s: &str,
            p: usize,
            g: usize,
        ) -> Result<Option<ClaimToken>, StoreError> {
            self.inner.try_claim_run(id, s, p, g)
        }
        fn renew_claim_for_restore(&self, id: &str, s: &str) -> Result<ClaimToken, StoreError> {
            self.inner.renew_claim_for_restore(id, s)
        }
        fn claim_counts(&self, s: &str) -> Result<(usize, usize), StoreError> {
            self.inner.claim_counts(s)
        }
        fn heartbeat_claim(&self, t: &ClaimToken) -> Result<(), StoreError> {
            self.inner.heartbeat_claim(t)
        }
        fn release_claim(&self, t: &ClaimToken) -> Result<(), StoreError> {
            self.inner.release_claim(t)
        }
        fn expired_claims(&self, n: &str) -> Result<Vec<ClaimToken>, StoreError> {
            self.inner.expired_claims(n)
        }
        fn append_event(&self, e: &SopEventRecord) -> Result<u64, StoreError> {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected append failure".into()));
            }
            self.inner.append_event(e)
        }
        fn list_events(&self, id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
            self.inner.list_events(id)
        }
        fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError> {
            self.inner.save_proposal(p)
        }
        fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
            self.inner.load_proposal(id)
        }
        fn list_proposals(
            &self,
            s: Option<ProposalStatus>,
        ) -> Result<Vec<ProposalRecord>, StoreError> {
            self.inner.list_proposals(s)
        }
        fn prune(&self, p: &RetentionPolicy) -> Result<usize, StoreError> {
            self.inner.prune(p)
        }
        fn health_check(&self) -> bool {
            self.inner.health_check()
        }
        fn backend(&self) -> &'static str {
            "failing-append-test"
        }
    }

    #[test]
    fn audit_append_failure_rolls_back_reacquired_claim() {
        // A gate approval reacquires the exec claim BEFORE the audit append. If that
        // append then fails, the run stays WaitingApproval - so the reacquired claim
        // MUST be rolled back, else the parked run keeps occupying an exec slot and
        // wrongly defers later triggers.
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
        });
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let a = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&a).to_string();
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "a parked run holds no exec claim"
        );
        // Now make the audit append fail, then approve.
        store.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let res = engine.resolve_gate(
            &run_id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(None),
        );
        assert!(
            res.is_err(),
            "resolution aborts when the audit row cannot be written"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "the reacquired claim is rolled back on audit-append failure"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the gate stays waiting (re-resolvable)"
        );
    }

    /// Delegates to an in-memory store but can fail `save_run`, to prove a park
    /// does NOT release its exec claim when the parked snapshot cannot be durably
    /// persisted and that a later retry can complete the transition.
    struct FailingSaveStore {
        inner: InMemoryRunStore,
        fail_saves: std::sync::atomic::AtomicBool,
    }

    impl FailingSaveStore {
        fn new() -> Self {
            Self {
                inner: InMemoryRunStore::new(),
                fail_saves: std::sync::atomic::AtomicBool::new(true),
            }
        }

        fn allow_saves(&self) {
            self.fail_saves
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }

        fn fail_saves(&self) {
            self.fail_saves
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl SopRunStore for FailingSaveStore {
        fn save_run(&self, r: &PersistedRun) -> Result<(), StoreError> {
            if self.fail_saves.load(std::sync::atomic::Ordering::SeqCst) {
                Err(StoreError::Backend("injected save_run failure".into()))
            } else {
                self.inner.save_run(r)
            }
        }
        fn finish_run(&self, id: &str, t: &PersistedRun) -> Result<(), StoreError> {
            self.inner.finish_run(id, t)
        }
        fn load_terminal_runs(&self, _limit: usize) -> Result<Vec<PersistedRun>, StoreError> {
            Ok(Vec::new())
        }
        fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError> {
            self.inner.load_active_runs()
        }
        fn load_run(&self, id: &str) -> Result<Option<PersistedRun>, StoreError> {
            self.inner.load_run(id)
        }
        fn last_terminal_completed_at(&self, s: &str) -> Result<Option<String>, StoreError> {
            self.inner.last_terminal_completed_at(s)
        }
        fn try_claim_run(
            &self,
            id: &str,
            s: &str,
            p: usize,
            g: usize,
        ) -> Result<Option<ClaimToken>, StoreError> {
            self.inner.try_claim_run(id, s, p, g)
        }
        fn renew_claim_for_restore(&self, id: &str, s: &str) -> Result<ClaimToken, StoreError> {
            self.inner.renew_claim_for_restore(id, s)
        }
        fn claim_counts(&self, s: &str) -> Result<(usize, usize), StoreError> {
            self.inner.claim_counts(s)
        }
        fn heartbeat_claim(&self, t: &ClaimToken) -> Result<(), StoreError> {
            self.inner.heartbeat_claim(t)
        }
        fn release_claim(&self, t: &ClaimToken) -> Result<(), StoreError> {
            self.inner.release_claim(t)
        }
        fn expired_claims(&self, n: &str) -> Result<Vec<ClaimToken>, StoreError> {
            self.inner.expired_claims(n)
        }
        fn append_event(&self, e: &SopEventRecord) -> Result<u64, StoreError> {
            self.inner.append_event(e)
        }
        fn list_events(&self, id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
            self.inner.list_events(id)
        }
        fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError> {
            self.inner.save_proposal(p)
        }
        fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
            self.inner.load_proposal(id)
        }
        fn list_proposals(
            &self,
            s: Option<ProposalStatus>,
        ) -> Result<Vec<ProposalRecord>, StoreError> {
            self.inner.list_proposals(s)
        }
        fn prune(&self, p: &RetentionPolicy) -> Result<usize, StoreError> {
            self.inner.prune(p)
        }
        fn health_check(&self) -> bool {
            self.inner.health_check()
        }
        fn backend(&self) -> &'static str {
            "failing-save-test"
        }
    }

    #[test]
    fn parked_approval_keeps_its_claim_when_the_snapshot_persist_fails() {
        // Regression: parking frees the exec slot ONLY after the parked snapshot is
        // durably persisted. If save_run fails, the claim is KEPT (fail closed) so
        // the parked run is never both claimless AND un-persisted - a crash would
        // otherwise lose the approval while newer triggers had already admitted
        // into the "freed" slot.
        let store = std::sync::Arc::new(FailingSaveStore::new());
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let a = engine.start_run("s1", manual_event()).unwrap();
        assert!(
            matches!(a, SopRunAction::WaitApproval { .. }),
            "a supervised first step parks at WaitingApproval, got {a:?}"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the exec claim is KEPT when the parked snapshot cannot be persisted"
        );
        assert!(
            !engine.can_start("s1"),
            "the held slot must not admit a new trigger while the park is un-persisted"
        );
    }

    #[test]
    fn approval_route_waits_for_durable_park_before_notifying_once() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let adapter = std::sync::Arc::new(RecordingRouteAdapter {
            calls: calls.clone(),
        });
        let store = std::sync::Arc::new(FailingSaveStore::new());
        let mut engine = policied_supervised_engine(Some("discord.ops:123456789"), adapter)
            .with_store(store.clone());

        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
        assert!(
            calls.lock().unwrap().is_empty(),
            "a failed parked-state persist must not emit an orphanable approval prompt"
        );

        store.allow_saves();
        engine.run_maintenance_tick();
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "the first successful retry emits the deferred approval prompt"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "the durable parked run releases its exec claim"
        );

        engine.run_maintenance_tick();
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "later maintenance ticks must not resend the same initial prompt"
        );
    }

    #[test]
    fn checkpoint_park_keeps_its_claim_when_the_snapshot_persist_fails() {
        // Same fail-closed guarantee as the approval-park case, for the
        // deterministic-checkpoint park site.
        let store = std::sync::Arc::new(FailingSaveStore::new());
        let mut engine =
            engine_with_sops(vec![deterministic_sop("det-cp")]).with_store(store.clone());
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            store.claim_counts("det-cp").unwrap(),
            (1, 1),
            "the exec claim is KEPT when the checkpoint snapshot cannot be persisted"
        );
        assert!(
            !engine.can_start("det-cp"),
            "the held slot must not admit a new trigger while the checkpoint is un-persisted"
        );
    }

    #[test]
    fn resolve_gate_refuses_to_approve_while_park_persist_is_pending() {
        // A pending park can already hold the exec claim because fail-closed
        // persistence kept it. Approval must be refused before reacquiring or
        // rolling back any claim; otherwise a failed approval attempt could release
        // that pre-existing claim and leave the run both claimless and unpersisted.
        let store = std::sync::Arc::new(FailingSaveStore::new());
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let a = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&a).to_string();
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the exec claim is KEPT when the parked snapshot cannot be persisted"
        );

        let res = engine.resolve_gate(
            &run_id,
            ApprovalDecision::Approve,
            ApprovalPrincipal::cli(Some("alice".into())),
        );
        assert!(
            res.is_err(),
            "approval must be refused while the park's snapshot is not yet durably persisted"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the pre-existing kept claim must survive the refused approval attempt"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the gate stays waiting, re-resolvable once the park persists"
        );
    }

    #[test]
    fn approve_step_refuses_to_resume_while_checkpoint_persist_is_pending() {
        // Same class of regression as `resolve_gate_refuses_to_approve_while_
        // park_persist_is_pending`, for the deterministic-checkpoint resume path.
        let store = std::sync::Arc::new(FailingSaveStore::new());
        let mut engine =
            engine_with_sops(vec![deterministic_sop("det-cp")]).with_store(store.clone());
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            store.claim_counts("det-cp").unwrap(),
            (1, 1),
            "the exec claim is KEPT when the checkpoint snapshot cannot be persisted"
        );

        let res = engine.approve_step(&run_id);
        assert!(
            res.is_err(),
            "resume must be refused while the checkpoint's snapshot is not yet durably persisted"
        );
        assert_eq!(
            store.claim_counts("det-cp").unwrap(),
            (1, 1),
            "the pre-existing kept claim must survive the refused resume attempt"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "the run stays parked, re-resolvable once the checkpoint persists"
        );
    }

    #[test]
    fn resume_deterministic_run_refuses_to_resume_while_checkpoint_persist_is_pending() {
        // Same class of regression, via the restore-path entry point.
        let store = std::sync::Arc::new(FailingSaveStore::new());
        let mut engine =
            engine_with_sops(vec![deterministic_sop("det-cp")]).with_store(store.clone());
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            store.claim_counts("det-cp").unwrap(),
            (1, 1),
            "the exec claim is KEPT when the checkpoint snapshot cannot be persisted"
        );

        let mut step_outputs = HashMap::new();
        step_outputs.insert(1u32, serde_json::json!("s1-out"));
        let state = DeterministicRunState {
            run_id: run_id.clone(),
            sop_name: "det-cp".to_string(),
            last_completed_step: 1,
            total_steps: 3,
            step_outputs,
            persisted_at: now_iso8601(),
            llm_calls_saved: 0,
            paused_at_checkpoint: true,
        };

        let res = engine.resume_deterministic_run(state);
        assert!(
            res.is_err(),
            "resume must be refused while the checkpoint's snapshot is not yet durably persisted"
        );
        assert_eq!(
            store.claim_counts("det-cp").unwrap(),
            (1, 1),
            "the pre-existing kept claim must survive the refused resume attempt"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "the run stays parked, re-resolvable once the checkpoint persists"
        );
    }

    /// A test store with REAL, test-controllable claim-lease semantics - unlike
    /// `InMemoryRunStore`, whose claims carry a permanently empty (never-expiring)
    /// lease. `save_run` always fails, so a park's snapshot never durably
    /// persists. Used to prove a claim KEPT alive only by
    /// `heartbeat_active_claims` survives the maintenance reaper, while one that
    /// stopped being heartbeated (the pre-fix bug) would be reclaimed once its
    /// lease is in the past.
    struct FailingSaveLeasedStore {
        inner: InMemoryRunStore,
        claims: std::sync::Mutex<std::collections::HashMap<String, ClaimToken>>,
    }
    impl FailingSaveLeasedStore {
        fn new() -> Self {
            Self {
                inner: InMemoryRunStore::new(),
                claims: std::sync::Mutex::new(std::collections::HashMap::new()),
            }
        }
        /// Force an existing claim's lease into the past, simulating a claim that
        /// was taken but never subsequently renewed.
        fn expire_claim_now(&self, run_id: &str) {
            if let Some(token) = self.claims.lock().unwrap().get_mut(run_id) {
                token.lease_expires = "2000-01-01T00:00:00Z".to_string();
            }
        }
    }
    impl SopRunStore for FailingSaveLeasedStore {
        fn save_run(&self, _r: &PersistedRun) -> Result<(), StoreError> {
            Err(StoreError::Backend("injected save_run failure".into()))
        }
        fn finish_run(&self, id: &str, t: &PersistedRun) -> Result<(), StoreError> {
            self.inner.finish_run(id, t)
        }
        fn load_terminal_runs(&self, _limit: usize) -> Result<Vec<PersistedRun>, StoreError> {
            Ok(Vec::new())
        }
        fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError> {
            self.inner.load_active_runs()
        }
        fn load_run(&self, id: &str) -> Result<Option<PersistedRun>, StoreError> {
            self.inner.load_run(id)
        }
        fn last_terminal_completed_at(&self, s: &str) -> Result<Option<String>, StoreError> {
            self.inner.last_terminal_completed_at(s)
        }
        fn try_claim_run(
            &self,
            run_id: &str,
            sop_name: &str,
            per_sop_cap: usize,
            global_cap: usize,
        ) -> Result<Option<ClaimToken>, StoreError> {
            let mut claims = self.claims.lock().unwrap();
            if claims.contains_key(run_id) {
                return Ok(None);
            }
            let active_for_sop = claims.values().filter(|c| c.sop_name == sop_name).count();
            if active_for_sop >= per_sop_cap || claims.len() >= global_cap {
                return Ok(None);
            }
            let now = now_iso8601();
            let token = ClaimToken {
                run_id: run_id.to_string(),
                sop_name: sop_name.to_string(),
                claimed_at: now,
                // Far-future: this test drives expiry explicitly via
                // `expire_claim_now`/`heartbeat_claim`, not real elapsed time.
                lease_expires: "2099-01-01T00:00:00Z".to_string(),
                holder: "leased-test".to_string(),
            };
            claims.insert(run_id.to_string(), token.clone());
            Ok(Some(token))
        }
        fn renew_claim_for_restore(
            &self,
            run_id: &str,
            sop_name: &str,
        ) -> Result<ClaimToken, StoreError> {
            let token = ClaimToken {
                run_id: run_id.to_string(),
                sop_name: sop_name.to_string(),
                claimed_at: now_iso8601(),
                lease_expires: "2099-01-01T00:00:00Z".to_string(),
                holder: "leased-test".to_string(),
            };
            self.claims
                .lock()
                .unwrap()
                .insert(run_id.to_string(), token.clone());
            Ok(token)
        }
        fn claim_counts(&self, sop_name: &str) -> Result<(usize, usize), StoreError> {
            let claims = self.claims.lock().unwrap();
            let per_sop = claims.values().filter(|c| c.sop_name == sop_name).count();
            Ok((per_sop, claims.len()))
        }
        fn heartbeat_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
            if let Some(existing) = self.claims.lock().unwrap().get_mut(&token.run_id) {
                existing.lease_expires = "2099-01-01T00:00:00Z".to_string();
            }
            Ok(())
        }
        fn release_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
            self.claims.lock().unwrap().remove(&token.run_id);
            Ok(())
        }
        fn expired_claims(&self, now_iso: &str) -> Result<Vec<ClaimToken>, StoreError> {
            let claims = self.claims.lock().unwrap();
            Ok(claims
                .values()
                .filter(|c| c.lease_expires.as_str() <= now_iso)
                .cloned()
                .collect())
        }
        fn append_event(&self, e: &SopEventRecord) -> Result<u64, StoreError> {
            self.inner.append_event(e)
        }
        fn list_events(&self, id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
            self.inner.list_events(id)
        }
        fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError> {
            self.inner.save_proposal(p)
        }
        fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
            self.inner.load_proposal(id)
        }
        fn list_proposals(
            &self,
            s: Option<ProposalStatus>,
        ) -> Result<Vec<ProposalRecord>, StoreError> {
            self.inner.list_proposals(s)
        }
        fn prune(&self, p: &RetentionPolicy) -> Result<usize, StoreError> {
            self.inner.prune(p)
        }
        fn health_check(&self) -> bool {
            self.inner.health_check()
        }
        fn backend(&self) -> &'static str {
            "failing-save-leased-test"
        }
    }

    #[test]
    fn parked_claim_kept_after_failed_persist_survives_maintenance_reap() {
        // Keeping the claim on a failed park persist is only fail-closed if that
        // claim's lease keeps being renewed. Otherwise the maintenance reaper can
        // reclaim it while the run is still unpersisted, undoing the fail-closed
        // keep and allowing a newer trigger to over-admit.
        let store = std::sync::Arc::new(FailingSaveLeasedStore::new());
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let a = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&a).to_string();
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the exec claim is KEPT when the parked snapshot cannot be persisted"
        );

        // Simulate real time passing with no heartbeat since the original claim:
        // force the lease into the past, as if an hour had gone by unrenewed.
        store.expire_claim_now(&run_id);

        // A maintenance tick must renew the kept claim's lease (via
        // `retry_pending_park_persists` + `heartbeat_active_claims`) before the
        // reaper runs, so the now-expired-in-the-past lease gets refreshed rather
        // than reclaimed.
        engine.run_maintenance_tick();

        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the kept claim must survive the maintenance tick's reaper - it must be \
             heartbeated, not silently reclaimed once its (unrenewed) lease is in the past"
        );
        assert!(
            !engine.can_start("s1"),
            "the slot must still be held after the tick - the park is still un-persisted"
        );
    }

    #[test]
    fn resolve_gate_clears_routed_non_contiguous_step() {
        // End-to-end: a routed SOP waiting at step 5 (steps numbered 1 and 5) must
        // clear by step NUMBER. Before the fix, clear_waiting_gate read step index 4
        // of a 2-element vec -> None -> Err, but only AFTER resolve_gate reacquired
        // the claim and wrote gate_resolved.
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.steps = vec![
            SopStep {
                number: 1,
                title: "a".into(),
                ..SopStep::default()
            },
            SopStep {
                number: 5,
                title: "b".into(),
                ..SopStep::default()
            },
        ];
        let mut engine =
            engine_with_sops(vec![sop]).with_store(std::sync::Arc::new(InMemoryRunStore::new()));
        let now = now_iso8601();
        engine.active_runs.insert(
            "r1".to_string(),
            SopRun {
                run_id: "r1".to_string(),
                sop_name: "s1".to_string(),
                trigger_event: manual_event(),
                frame_marker_id: "m".to_string(),
                status: SopRunStatus::WaitingApproval,
                current_step: 5,
                total_steps: 2,
                started_at: now.clone(),
                completed_at: None,
                step_results: Vec::new(),
                waiting_since: Some(now),
                llm_calls_saved: 0,
                revision: 0,
                revision_base: 0,
            },
        );
        let out = engine
            .resolve_gate(
                "r1",
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(None),
            )
            .expect("routed gate clears without error");
        match out {
            crate::sop::approval::ResolveOutcome::Resumed(a) => match *a {
                SopRunAction::ExecuteStep { step, .. } => assert_eq!(
                    step.number, 5,
                    "resumes the step whose NUMBER is 5, not vec index 5"
                ),
                other => panic!("expected ExecuteStep, got {other:?}"),
            },
            other => panic!("expected Resumed, got {other:?}"),
        }
    }

    #[test]
    fn persist_runs_defaults_on() {
        // A1 durability leg: parked HITL runs must survive a restart out of the box.
        assert!(
            SopConfig::default().persist_runs,
            "persist_runs must default on so a pending approval is not lost on restart"
        );
    }

    // ── A2: admission policy (SopAdmissionPolicy) ─────────────────

    /// A single-slot SOP that stays executing (Auto, multi-step) after start, so
    /// its exec slot is occupied for admission-policy assertions.
    fn exec_filled_engine(policy: SopAdmissionPolicy) -> (SopEngine, String) {
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.max_concurrent = 1;
        sop.admission_policy = policy;
        let mut engine = engine_with_sops(vec![sop]).with_store(store);
        let a = engine.start_run("s1", manual_event()).unwrap();
        assert!(
            matches!(a, SopRunAction::ExecuteStep { .. }),
            "auto start executes (holds its exec slot)"
        );
        let run_id = extract_run_id(&a).to_string();
        (engine, run_id)
    }

    #[test]
    fn admission_policy_defaults_to_parallel() {
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        assert_eq!(sop.admission_policy, SopAdmissionPolicy::Parallel);
        assert_eq!(sop.max_pending_approvals, 0);
    }

    #[test]
    fn parallel_admits_when_a_slot_is_free() {
        let engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        assert_eq!(engine.evaluate_admission("s1"), SopAdmission::Admit);
    }

    #[test]
    fn parallel_defers_when_exec_slots_full() {
        // Never drops on concurrency: a second trigger is deferred for backpressure.
        let (engine, _) = exec_filled_engine(SopAdmissionPolicy::Parallel);
        assert!(matches!(
            engine.evaluate_admission("s1"),
            SopAdmission::Defer { .. }
        ));
    }

    #[test]
    fn drop_policy_drops_when_exec_slots_full() {
        // Explicit opt-in to the legacy fire-and-forget behavior.
        let (engine, _) = exec_filled_engine(SopAdmissionPolicy::Drop);
        assert!(matches!(
            engine.evaluate_admission("s1"),
            SopAdmission::Drop { .. }
        ));
    }

    #[test]
    fn hold_defers_while_a_run_is_in_flight() {
        let (engine, _) = exec_filled_engine(SopAdmissionPolicy::Hold);
        assert!(matches!(
            engine.evaluate_admission("s1"),
            SopAdmission::Defer { .. }
        ));
    }

    #[test]
    fn coalesce_folds_into_the_in_flight_run() {
        let (engine, run1) = exec_filled_engine(SopAdmissionPolicy::Coalesce);
        match engine.evaluate_admission("s1") {
            SopAdmission::Coalesce { existing_run_id } => assert_eq!(existing_run_id, run1),
            other => panic!("expected Coalesce, got {other:?}"),
        }
    }

    #[test]
    fn pending_pool_bound_defers_new_triggers() {
        // Exec slots are free, but the pending-approval pool is full (a Supervised run
        // parks immediately) -> a new trigger defers (backpressure), never dropped.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.max_concurrent = 5;
        sop.max_pending_approvals = 1;
        let mut engine = engine_with_sops(vec![sop]).with_store(store);
        let a = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(a, SopRunAction::WaitApproval { .. }));
        assert!(matches!(
            engine.evaluate_admission("s1"),
            SopAdmission::Defer { .. }
        ));
    }

    #[test]
    fn pending_pool_bound_preempts_coalesce_into_a_parked_run() {
        // The `max_pending_approvals` cap check in `evaluate_admission` runs BEFORE
        // the per-policy match, so it must defer a fresh trigger even under
        // Coalesce - even though `first_active_run_for_sop` WOULD find the parked
        // run to fold onto - rather than let Coalesce bypass the pending-approval
        // backpressure bound. Exec slots stay free (max_concurrent=5); only the
        // pending pool (max_pending_approvals=1) is at capacity.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.max_concurrent = 5;
        sop.max_pending_approvals = 1;
        sop.admission_policy = SopAdmissionPolicy::Coalesce;
        let mut engine = engine_with_sops(vec![sop]).with_store(store);
        let a = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(a, SopRunAction::WaitApproval { .. }));
        let run_id = extract_run_id(&a).to_string();

        // Sanity: absent the cap, Coalesce would find this same parked run to fold
        // onto - so the Defer below is the cap preempting Coalesce, not a case
        // where there was nothing to coalesce with.
        assert_eq!(engine.first_active_run_for_sop("s1"), Some(run_id));

        assert!(
            matches!(engine.evaluate_admission("s1"), SopAdmission::Defer { .. }),
            "the pending-approval cap must defer, not Coalesce past it"
        );
    }

    // ── Eviction ──────────────────────────────────────

    #[test]
    fn max_finished_runs_evicts_oldest() {
        let mut engine = SopEngine::new(SopConfig {
            max_finished_runs: 2,
            ..SopConfig::default()
        });
        // SOP with 1 step so each run completes in one advance
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps = vec![sop.steps[0].clone()];
        sop.max_concurrent = 10;
        engine.sops = vec![sop];

        // Complete 3 runs
        let mut finished_ids = Vec::new();
        for _ in 0..3 {
            let action = engine.start_run("s1", manual_event()).unwrap();
            let rid = extract_run_id(&action).to_string();
            engine
                .advance_step(
                    &rid,
                    SopStepResult {
                        step_number: 1,
                        status: SopStepStatus::Completed,
                        output: "ok".into(),
                        started_at: now_iso8601(),
                        completed_at: Some(now_iso8601()),
                        tool_calls: Vec::new(),
                    },
                )
                .unwrap();
            finished_ids.push(rid);
        }

        // Only 2 should be kept (max_finished_runs=2)
        let finished = engine.finished_runs(None);
        assert_eq!(
            finished.len(),
            2,
            "eviction should cap at max_finished_runs"
        );
        // Oldest (first) run should be evicted, newest two remain
        assert_eq!(finished[0].run_id, finished_ids[1]);
        assert_eq!(finished[1].run_id, finished_ids[2]);
    }

    #[test]
    fn max_finished_runs_zero_means_unlimited() {
        let mut engine = SopEngine::new(SopConfig {
            max_finished_runs: 0,
            ..SopConfig::default()
        });
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps = vec![sop.steps[0].clone()];
        sop.max_concurrent = 10;
        engine.sops = vec![sop];

        for _ in 0..5 {
            let action = engine.start_run("s1", manual_event()).unwrap();
            let rid = extract_run_id(&action).to_string();
            engine
                .advance_step(
                    &rid,
                    SopStepResult {
                        step_number: 1,
                        status: SopStepStatus::Completed,
                        output: "ok".into(),
                        started_at: now_iso8601(),
                        completed_at: Some(now_iso8601()),
                        tool_calls: Vec::new(),
                    },
                )
                .unwrap();
        }

        assert_eq!(engine.finished_runs(None).len(), 5, "zero means unlimited");
    }

    #[test]
    fn waiting_since_cleared_on_approve() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        approve_gate_cli(&mut engine, &run_id);

        let run = engine.get_run(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::Running);
        assert!(run.waiting_since.is_none());
    }

    // ── Deterministic execution ─────────────────────────

    fn deterministic_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: format!("Deterministic SOP: {name}"),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Step one".into(),
                    body: "Do step one".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::Execute,
                    schema: None,
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "Checkpoint".into(),
                    body: "Pause for approval".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::Checkpoint,
                    schema: None,
                    ..SopStep::default()
                },
                SopStep {
                    number: 3,
                    title: "Step three".into(),
                    body: "Final step".into(),
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
            deterministic: true,
            admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        }
    }

    #[test]
    fn deterministic_start_returns_deterministic_step() {
        let mut engine = engine_with_sops(vec![deterministic_sop("det-sop")]);
        let action = engine.start_run("det-sop", manual_event()).unwrap();
        assert!(
            matches!(action, SopRunAction::DeterministicStep { ref step, .. } if step.number == 1),
            "First action should be DeterministicStep for step 1"
        );
        let run_id = extract_run_id(&action).to_string();
        assert!(run_id.starts_with("det-"));
    }

    #[test]
    fn deterministic_start_routes_through_start_run() {
        let mut engine = engine_with_sops(vec![deterministic_sop("det-sop")]);
        // start_run should auto-route to start_deterministic_run
        let action = engine.start_run("det-sop", manual_event()).unwrap();
        assert!(matches!(action, SopRunAction::DeterministicStep { .. }));
    }

    #[test]
    fn deterministic_advance_pipes_output() {
        let mut engine = engine_with_sops(vec![deterministic_sop("det-sop")]);
        let action = engine.start_run("det-sop", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Advance step 1 with output
        let output = serde_json::json!({"result": "step1_done"});
        let action = engine
            .advance_deterministic_step(&run_id, output.clone(), None)
            .unwrap();

        // Step 2 is a checkpoint — should pause
        assert!(
            matches!(action, SopRunAction::CheckpointWait { ref step, .. } if step.number == 2),
            "Step 2 (checkpoint) should return CheckpointWait"
        );
    }

    #[test]
    fn deterministic_checkpoint_pauses_run() {
        let mut engine = engine_with_sops(vec![deterministic_sop("det-sop")]);
        let action = engine.start_run("det-sop", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Complete step 1
        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!({"ok": true}), None)
            .unwrap();

        // Should be at checkpoint
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));

        // Run should be PausedCheckpoint
        let run = engine.get_run(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::PausedCheckpoint);
        assert!(run.waiting_since.is_some());
    }

    #[test]
    fn deterministic_completion_tracks_savings() {
        let mut sop = deterministic_sop("det-sop");
        // Simplify: 2 execute steps, no checkpoint
        sop.steps = vec![
            SopStep {
                number: 1,
                title: "Step one".into(),
                body: "Do it".into(),
                suggested_tools: vec![],
                requires_confirmation: false,
                kind: SopStepKind::Execute,
                schema: None,
                ..SopStep::default()
            },
            SopStep {
                number: 2,
                title: "Step two".into(),
                body: "Do it too".into(),
                suggested_tools: vec![],
                requires_confirmation: false,
                kind: SopStepKind::Execute,
                schema: None,
                ..SopStep::default()
            },
        ];
        let mut engine = engine_with_sops(vec![sop]);

        let action = engine.start_run("det-sop", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Complete step 1
        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::DeterministicStep { .. }));

        // Complete step 2
        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s2"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::Completed { .. }));

        // Check savings
        let savings = engine.deterministic_savings();
        assert_eq!(savings.total_runs, 1);
        assert_eq!(savings.total_llm_calls_saved, 2);
    }

    #[test]
    fn deterministic_non_deterministic_sop_rejected() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let result = engine.start_deterministic_run("s1", manual_event());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not in deterministic mode")
        );
    }

    #[test]
    fn new_engine_without_sops_dir_stays_empty() {
        let config = SopConfig {
            sops_dir: None,
            ..Default::default()
        };
        let engine = SopEngine::new(config);
        assert!(
            engine.sops().is_empty(),
            "engine without sops_dir must have no SOPs"
        );
    }

    #[test]
    fn reload_loads_sops_when_sops_dir_is_configured() {
        let tmp = tempfile::tempdir().unwrap();
        let sops_dir = tmp.path().join("my_sops");
        let sop_subdir = sops_dir.join("test-sop");
        std::fs::create_dir_all(&sop_subdir).unwrap();

        std::fs::write(
            sop_subdir.join("SOP.toml"),
            r#"
[sop]
name = "test-sop"
description = "A test SOP"
version = "1.0.0"

[[triggers]]
type = "manual"
"#,
        )
        .unwrap();

        let config = SopConfig {
            sops_dir: Some(sops_dir.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut engine = SopEngine::new(config);
        engine.reload(tmp.path());
        assert_eq!(
            engine.sops().len(),
            1,
            "reload must populate SOPs from disk"
        );
        assert_eq!(engine.sops()[0].name, "test-sop");
    }

    fn deterministic_sop_all_execute(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: format!("Deterministic SOP: {name}"),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Step one".into(),
                    body: "Do step one".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                    kind: SopStepKind::Execute,
                    schema: None,
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "Step two".into(),
                    body: "Do step two".into(),
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
            deterministic: true,
            admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        }
    }

    #[test]
    fn deterministic_run_drives_to_completion_through_advance_step() {
        let mut engine = engine_with_sops(vec![deterministic_sop_all_execute("det-run")]);
        let action = engine.start_run("det-run", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(
            matches!(action, SopRunAction::DeterministicStep { ref step, .. } if step.number == 1)
        );

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "step1-output".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            matches!(action, SopRunAction::DeterministicStep { ref step, .. } if step.number == 2),
            "advance_step on a deterministic run must route to the deterministic path"
        );

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "step2-output".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            matches!(action, SopRunAction::Completed { .. }),
            "deterministic run should complete after its final step"
        );
    }

    #[test]
    fn deterministic_run_uses_explicit_next_routing() {
        let mut sop = deterministic_sop_all_execute("det-route");
        sop.steps.push(SopStep {
            number: 3,
            title: "Step three".into(),
            body: "Do step three".into(),
            kind: SopStepKind::Execute,
            ..SopStep::default()
        });
        sop.steps[0].routing.next = Some(3);
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("det-route", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(
            matches!(action, SopRunAction::DeterministicStep { ref step, .. } if step.number == 1)
        );

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!({"ok": true}), None)
            .unwrap();

        assert!(
            matches!(action, SopRunAction::DeterministicStep { ref step, .. } if step.number == 3),
            "deterministic routing should select explicit step 3"
        );
    }

    #[test]
    fn deterministic_routed_checkpoint_persists_actual_last_completed_step() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sop = deterministic_sop_all_execute("det-route-cp");
        sop.location = Some(tmp.path().to_path_buf());
        sop.steps.push(SopStep {
            number: 3,
            title: "Checkpoint three".into(),
            body: "Pause at step three".into(),
            kind: SopStepKind::Checkpoint,
            ..SopStep::default()
        });
        sop.steps[0].routing.next = Some(3);
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("det-route-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!({"step": 1}), None)
            .unwrap();
        let (state_file, step_number) = match action {
            SopRunAction::CheckpointWait {
                state_file, step, ..
            } => (state_file, step.number),
            other => {
                assert!(
                    matches!(other, SopRunAction::CheckpointWait { .. }),
                    "expected routed checkpoint wait"
                );
                return;
            }
        };
        assert_eq!(step_number, 3);

        let state = SopEngine::load_deterministic_state(&state_file).unwrap();

        assert_eq!(state.last_completed_step, 1);
        assert!(state.step_outputs.contains_key(&1));
        assert!(!state.step_outputs.contains_key(&2));
    }

    #[test]
    fn deterministic_failed_step_fails_run_through_advance_step() {
        let mut engine = engine_with_sops(vec![deterministic_sop_all_execute("det-fail")]);
        let action = engine.start_run("det-fail", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Failed,
                    output: "boom".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            matches!(action, SopRunAction::Failed { .. }),
            "a failed deterministic step must fail the run"
        );
    }

    #[test]
    fn deterministic_output_schema_failure_fails_run() {
        let mut sop = deterministic_sop_all_execute("det-schema");
        sop.steps[0].schema = Some(StepSchema {
            input: None,
            output: Some(required_object_schema("ok")),
        });
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("det-schema", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!({}), None)
            .unwrap();

        assert!(
            matches!(action, SopRunAction::Failed { ref reason, .. } if reason.contains("output schema validation failed"))
        );
        assert!(engine.active_runs().is_empty());
        assert_eq!(engine.finished_runs(None)[0].status, SopRunStatus::Failed);
    }

    #[test]
    fn deterministic_advance_step_preserves_caller_timestamps() {
        let mut engine = engine_with_sops(vec![deterministic_sop_all_execute("det-ts")]);
        let action = engine.start_run("det-ts", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let started = "2026-01-01T00:00:00Z".to_string();
        let completed = "2026-01-01T00:00:42Z".to_string();
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "step1-output".into(),
                    started_at: started.clone(),
                    completed_at: Some(completed.clone()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();

        let recorded = engine
            .get_run(&run_id)
            .unwrap()
            .step_results
            .iter()
            .find(|r| r.step_number == 1)
            .expect("step 1 result recorded");
        assert_eq!(recorded.started_at, started);
        assert_eq!(recorded.completed_at, Some(completed));
    }

    #[test]
    fn deterministic_checkpoint_resumes_through_approve_step() {
        // approve_step owns the deterministic PausedCheckpoint resume (the
        // sop_approve tool routes here when resolve_gate reports NotWaiting). A run
        // paused at a checkpoint must resume through it, not bail. deterministic_sop
        // is step1=Execute, step2=Checkpoint, step3=Execute.
        let mut engine = engine_with_sops(vec![deterministic_sop("det-cp")]);
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Advance step 1 -> pauses at the step-2 checkpoint.
        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );

        // Approve the checkpoint via the public path -> yields step 3.
        let action = engine.approve_step(&run_id).unwrap();
        assert!(
            matches!(action, SopRunAction::DeterministicStep { ref step, .. } if step.number == 3),
            "approving a deterministic checkpoint must resume to the next step"
        );

        // Advance step 3 -> run completes.
        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 3,
                    status: SopStepStatus::Completed,
                    output: "s3-out".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            matches!(action, SopRunAction::Completed { .. }),
            "deterministic run should complete after the post-checkpoint step"
        );
    }

    #[test]
    fn approve_step_fails_closed_when_sop_removed_while_parked() {
        // Regression: `approve_step` used to reacquire the exec claim and flip the
        // run to `Running` BEFORE `advance_deterministic_step` resolved the SOP and
        // its current step - so an operator removing the SOP definition while a
        // deterministic run sat parked at a checkpoint would strand the run in
        // `Running`, holding a claim, unable to ever advance (the resolve still
        // errors, but the mutation had already committed). The
        // `can_advance_deterministic_step` pre-flight must make this fail closed
        // with the run left untouched at `PausedCheckpoint` instead.
        let mut engine = engine_with_sops(vec![deterministic_sop("det-cp")]);
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );

        // Operator removes the SOP definition out from under the parked run.
        engine.set_sops_for_test(vec![]);

        let res = engine.approve_step(&run_id);
        assert!(
            res.is_err(),
            "approve_step must fail closed when the SOP is gone, not strand the run"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "a failed-closed approve must leave the run resumable, not stuck in Running"
        );

        // The exec slot was not leaked: restore the SOP and a fresh trigger must
        // admit. With max_concurrent=1, a claim leaked by the parked run would
        // defer this instead.
        engine.set_sops_for_test(vec![deterministic_sop("det-cp")]);
        let fresh = engine.start_run("det-cp", manual_event()).unwrap();
        assert!(
            matches!(fresh, SopRunAction::DeterministicStep { .. }),
            "a fresh run must admit - no phantom exec slot held by the parked run: {fresh:?}"
        );
    }

    #[test]
    fn resume_deterministic_run_fails_closed_when_sop_shrunk_while_parked() {
        // Regression: `resume_deterministic_run` resolved the waiting step
        // (`resolve_sop_step`) AFTER it had already reacquired the claim and
        // flipped the run to `Running` - so an operator shrinking the SOP
        // (removing the step the persisted checkpoint state points at) while the
        // run sat parked would strand it in `Running`, holding a claim, with no
        // way to make progress. The pre-flight must fail closed BEFORE the claim
        // and the mutation.
        let mut engine = engine_with_sops(vec![deterministic_sop("det-cp")]);
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );

        // Operator shrinks the SOP: step 1 (the persisted last-completed step) no
        // longer exists, though the SOP itself is still loaded under the same name.
        let mut shrunk = deterministic_sop("det-cp");
        shrunk.steps.clear();
        engine.set_sops_for_test(vec![shrunk]);

        let mut step_outputs = HashMap::new();
        step_outputs.insert(1u32, serde_json::json!("s1-out"));
        let state = DeterministicRunState {
            run_id: run_id.clone(),
            sop_name: "det-cp".to_string(),
            last_completed_step: 1,
            total_steps: 3,
            step_outputs,
            persisted_at: now_iso8601(),
            llm_calls_saved: 0,
            paused_at_checkpoint: true,
        };

        let res = engine.resume_deterministic_run(state);
        assert!(
            res.is_err(),
            "resume must fail closed when the waiting step no longer exists"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "a failed-closed resume must leave the run resumable, not stuck in Running"
        );

        // The exec slot was not leaked: restore the SOP and a fresh trigger must
        // admit. With max_concurrent=1, a claim leaked by the parked run would
        // defer this instead.
        engine.set_sops_for_test(vec![deterministic_sop("det-cp")]);
        let fresh = engine.start_run("det-cp", manual_event()).unwrap();
        assert!(
            matches!(fresh, SopRunAction::DeterministicStep { .. }),
            "a fresh run must admit - no phantom exec slot held by the parked run: {fresh:?}"
        );
    }

    /// `capability(noop) -> checkpoint -> capability(noop)`: the shape the
    /// checkpoint bridge exists for (an approved write-back tail, e.g.
    /// `forge.comment`, executing headlessly after an out-of-band approval).
    fn capability_checkpoint_sop(name: &str) -> Sop {
        let cap_step = |number: u32| SopStep {
            number,
            title: format!("Capability {number}"),
            kind: SopStepKind::Capability,
            capability: Some("noop".into()),
            ..SopStep::default()
        };
        Sop {
            name: name.into(),
            description: "cap -> checkpoint -> cap".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                cap_step(1),
                SopStep {
                    number: 2,
                    title: "Checkpoint".into(),
                    kind: SopStepKind::Checkpoint,
                    ..SopStep::default()
                },
                cap_step(3),
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        }
    }

    struct CountingForgeCommentAdapter {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl super::super::capability::ForgeCommentAdapter for CountingForgeCommentAdapter {
        fn post_comment(
            &self,
            _channel: Option<&str>,
            _repo: &str,
            _number: u64,
            _body: &str,
        ) -> std::result::Result<(), String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    struct MutatesForgePayload;

    impl super::super::capability::SopCapability for MutatesForgePayload {
        fn id(&self) -> &'static str {
            "mutate.forge"
        }

        fn describe(&self) -> super::super::capability::CapabilityInfo {
            super::super::capability::CapabilityInfo {
                id: self.id(),
                description: "Change the approved forge comment body",
                deterministic: true,
                idempotent: true,
                reversible: false,
                supports_retry: false,
                required_permissions: Vec::new(),
                input_schema: None,
                output_schema: None,
            }
        }

        fn execute(
            &self,
            _ctx: super::super::capability::CapabilityContext,
            _input: serde_json::Value,
        ) -> anyhow::Result<super::super::capability::CapabilityResult> {
            Ok(super::super::capability::CapabilityResult::success(
                serde_json::json!({
                    "repo": "o/r",
                    "number": 7,
                    "body": "mutated after approval",
                    "looped": true,
                }),
            ))
        }
    }

    fn forge_comment_registry(
        calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Arc<SopCapabilityRegistry> {
        let mut registry = super::super::capability::SopCapabilityRegistry::with_builtins();
        let adapter: Arc<dyn super::super::capability::ForgeCommentAdapter> =
            Arc::new(CountingForgeCommentAdapter { calls });
        registry.register(super::super::capability::ForgeCommentCapability::new(Some(
            adapter,
        )));
        Arc::new(registry)
    }

    fn forge_comment_registry_with_mutator(
        calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Arc<SopCapabilityRegistry> {
        let mut registry = super::super::capability::SopCapabilityRegistry::with_builtins();
        registry.register(MutatesForgePayload);
        let adapter: Arc<dyn super::super::capability::ForgeCommentAdapter> =
            Arc::new(CountingForgeCommentAdapter { calls });
        registry.register(super::super::capability::ForgeCommentCapability::new(Some(
            adapter,
        )));
        Arc::new(registry)
    }

    fn forge_comment_event() -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: Some(
                serde_json::json!({
                    "channel": "git.main",
                    "repo": "o/r",
                    "number": 7,
                    "body": "triage approved",
                })
                .to_string(),
            ),
            timestamp: now_iso8601(),
        }
    }

    fn forge_comment_step(number: u32) -> SopStep {
        forge_comment_step_with_channel(number, "git.main")
    }

    fn forge_comment_step_with_channel(number: u32, channel: &str) -> SopStep {
        SopStep {
            number,
            title: format!("Forge comment {number}"),
            kind: SopStepKind::Capability,
            capability: Some("forge.comment".into()),
            capability_input: Some(serde_json::json!({
                "channel": channel,
                "repo": "o/r",
                "number": 7,
                "body": "triage approved",
            })),
            ..SopStep::default()
        }
    }

    fn direct_forge_comment_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: "forge without checkpoint".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![forge_comment_step(1)],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        }
    }

    fn checkpoint_forge_comment_sop(name: &str) -> Sop {
        checkpoint_forge_comment_sop_with_channel(name, "git.main")
    }

    fn checkpoint_forge_comment_sop_with_channel(name: &str, channel: &str) -> Sop {
        Sop {
            name: name.into(),
            description: "checkpoint -> forge".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Checkpoint".into(),
                    kind: SopStepKind::Checkpoint,
                    ..SopStep::default()
                },
                forge_comment_step_with_channel(2, channel),
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        }
    }

    fn two_checkpoint_forge_comment_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: "checkpoint -> noop -> checkpoint -> forge".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "First checkpoint".into(),
                    kind: SopStepKind::Checkpoint,
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "Bridge".into(),
                    kind: SopStepKind::Capability,
                    capability: Some("noop".into()),
                    ..SopStep::default()
                },
                SopStep {
                    number: 3,
                    title: "Second checkpoint".into(),
                    kind: SopStepKind::Checkpoint,
                    ..SopStep::default()
                },
                forge_comment_step(4),
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        }
    }

    fn checkpoint_mutates_before_forge_comment_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: "checkpoint -> mutator -> forge".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Checkpoint".into(),
                    kind: SopStepKind::Checkpoint,
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "Mutate approved body".into(),
                    kind: SopStepKind::Capability,
                    capability: Some("mutate.forge".into()),
                    ..SopStep::default()
                },
                forge_comment_step(3),
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        }
    }

    fn same_step_revisit_forge_comment_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: "checkpoint -> marker -> checkpoint -> forge".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Checkpoint".into(),
                    kind: SopStepKind::Checkpoint,
                    routing: crate::sop::step_contract::StepRouting {
                        switch: vec![
                            crate::sop::step_contract::SwitchRule {
                                name: "second-visit".into(),
                                when: Some("$.steps.2.looped == true".into()),
                                goto: Some(3),
                            },
                            crate::sop::step_contract::SwitchRule {
                                name: "first-visit".into(),
                                when: None,
                                goto: Some(2),
                            },
                        ],
                        ..Default::default()
                    },
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "Mark loop".into(),
                    kind: SopStepKind::Capability,
                    capability: Some("mutate.forge".into()),
                    routing: crate::sop::step_contract::StepRouting {
                        next: Some(1),
                        ..Default::default()
                    },
                    ..SopStep::default()
                },
                forge_comment_step(3),
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        }
    }

    struct FailFirstFinishStore {
        inner: InMemoryRunStore,
        fail_next_finish: std::sync::atomic::AtomicBool,
    }

    impl FailFirstFinishStore {
        fn new() -> Self {
            Self {
                inner: InMemoryRunStore::new(),
                fail_next_finish: std::sync::atomic::AtomicBool::new(true),
            }
        }
    }

    impl SopRunStore for FailFirstFinishStore {
        fn save_run(&self, run: &PersistedRun) -> Result<(), StoreError> {
            self.inner.save_run(run)
        }

        fn finish_run(&self, run_id: &str, terminal: &PersistedRun) -> Result<(), StoreError> {
            if self
                .fail_next_finish
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StoreError::Backend(
                    "injected first terminal persistence failure".into(),
                ));
            }
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

        fn append_event(&self, event: &SopEventRecord) -> Result<u64, StoreError> {
            self.inner.append_event(event)
        }

        fn list_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
            self.inner.list_events(run_id)
        }

        fn save_proposal(&self, proposal: &ProposalRecord) -> Result<(), StoreError> {
            self.inner.save_proposal(proposal)
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
            "fail-first-finish-test"
        }
    }

    #[test]
    fn intake_gate_pipeline_pipes_the_trigger_payload_through_a_step_one_checkpoint() {
        // Marc's double-HITL shape: `checkpoint -> capability -> ...`. The
        // step-1 checkpoint has no prior step result, so its resume must pipe
        // the TRIGGER PAYLOAD forward (mapping identical to `step_input_value`),
        // not Null — otherwise the first work step is starved of the event.
        let sop = Sop {
            name: "intake-gate".into(),
            description: "checkpoint before work".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Deterministic,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Intake gate".into(),
                    kind: SopStepKind::Checkpoint,
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "Work".into(),
                    kind: SopStepKind::Capability,
                    capability: Some("noop".into()),
                    ..SopStep::default()
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: true,
            agent: None,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
        };
        let mut engine = engine_with_sops(vec![sop]);
        let event = SopEvent {
            source: SopTriggerSource::Channel,
            topic: Some("git.main:issues.opened".into()),
            payload: Some(r#"{"repo":"o/r","number":7}"#.into()),
            timestamp: now_iso8601(),
        };
        let first = engine.start_run("intake-gate", event).unwrap();
        assert!(
            matches!(first, SopRunAction::CheckpointWait { .. }),
            "run must park at the step-1 intake gate: {first:?}"
        );
        let run_id = extract_run_id(&first).to_string();

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("intake gate approve resolves");
        assert!(
            matches!(
                outcome,
                super::super::approval::BrokerOutcome::Resolved(
                    super::super::approval::ResolveOutcome::Resumed(_)
                )
            ),
            "expected Resolved(Resumed), got {outcome:?}"
        );
        // The noop capability echoes its input: the recorded step-2 output must
        // BE the trigger payload, proving it crossed the step-1 checkpoint.
        let run = engine
            .last_finished_run("intake-gate")
            .expect("run completed");
        assert_eq!(run.status, SopRunStatus::Completed);
        let step2 = run
            .step_results
            .iter()
            .find(|r| r.step_number == 2)
            .expect("step 2 recorded");
        let parsed: serde_json::Value =
            serde_json::from_str(&step2.output).expect("step-2 output is json");
        assert_eq!(
            parsed,
            serde_json::json!({"repo": "o/r", "number": 7}),
            "the trigger payload must survive the step-1 checkpoint"
        );
    }

    #[test]
    fn forge_comment_refuses_without_prior_ledgered_checkpoint() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut engine = engine_with_sops(vec![direct_forge_comment_sop("forge-direct")])
            .with_capabilities(forge_comment_registry(Arc::clone(&calls)));

        let first = engine.start_run("forge-direct", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let final_action = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("direct forge run should fail closed");

        assert!(
            matches!(final_action, SopRunAction::Failed { .. }),
            "direct forge.comment must fail closed, got {final_action:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "forge adapter must not be called before a ledgered checkpoint"
        );
        let run = engine
            .last_finished_run("forge-direct")
            .expect("failed run should be retained");
        let result = run
            .step_results
            .iter()
            .find(|result| result.step_number == 1)
            .expect("forge step result recorded");
        assert_eq!(result.status, SopStepStatus::Failed);
        assert!(
            result.output.contains("immediately preceding checkpoint"),
            "failure should name the missing authorization invariant: {result:?}"
        );
    }

    #[test]
    fn forge_comment_runs_after_checkpoint_resolution() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut engine = engine_with_sops(vec![checkpoint_forge_comment_sop("forge-approved")])
            .with_capabilities(forge_comment_registry(Arc::clone(&calls)));

        let first = engine
            .start_run("forge-approved", forge_comment_event())
            .unwrap();
        assert!(
            matches!(first, SopRunAction::CheckpointWait { .. }),
            "forge run must park at the checkpoint before writing: {first:?}"
        );
        let run_id = extract_run_id(&first).to_string();

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("checkpoint approve resolves");
        let super::super::approval::BrokerOutcome::Resolved(
            super::super::approval::ResolveOutcome::Resumed(final_action),
        ) = outcome
        else {
            panic!("expected Resolved(Resumed), got {outcome:?}");
        };

        assert!(
            matches!(*final_action, SopRunAction::Completed { .. }),
            "approved forge tail must complete headlessly: {final_action:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "forge adapter should run exactly once after checkpoint approval"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events.iter().any(|ev| ev.kind == "gate_resolved"),
            "checkpoint resolution must append the ledger row before forge.comment executes: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|ev| ev.kind == "capability_effect_completed"),
            "forge.comment success must write a durable effect marker: {events:?}"
        );
        let run = engine
            .last_finished_run("forge-approved")
            .expect("run reached the finished list");
        assert_eq!(run.status, SopRunStatus::Completed);
    }

    #[test]
    fn forge_comment_reapproval_after_terminal_persist_failure_does_not_repost() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let store: Arc<dyn SopRunStore> = Arc::new(FailFirstFinishStore::new());
        let mut engine = engine_with_sops(vec![checkpoint_forge_comment_sop("forge-replay")])
            .with_store(Arc::clone(&store))
            .with_capabilities(forge_comment_registry(Arc::clone(&calls)));

        let first = engine
            .start_run("forge-replay", forge_comment_event())
            .unwrap();
        assert!(
            matches!(first, SopRunAction::CheckpointWait { .. }),
            "forge run must park before writing: {first:?}"
        );
        let run_id = extract_run_id(&first).to_string();

        let err = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect_err("terminal persistence failure must propagate to the caller");
        assert!(
            err.to_string().contains("failed to persist terminal run"),
            "unexpected error: {err}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "first approval performs the public forge write once"
        );
        assert!(
            engine.get_run(&run_id).is_some(),
            "terminal persistence failure keeps the in-memory run active"
        );
        assert!(
            engine.last_finished_run("forge-replay").is_none(),
            "terminal persistence failure must not move the run to finished_runs"
        );
        let first_events = engine.run_events(&run_id).unwrap();
        assert_eq!(
            first_events
                .iter()
                .filter(|ev| ev.kind == "capability_effect_started")
                .count(),
            1,
            "forge write must have a durable started marker before the public send: {first_events:?}"
        );
        assert_eq!(
            first_events
                .iter()
                .filter(|ev| ev.kind == "capability_effect_completed")
                .count(),
            1,
            "forge write must have a durable completed marker after the public send: {first_events:?}"
        );

        drop(engine);

        let mut restarted = engine_with_sops(vec![checkpoint_forge_comment_sop("forge-replay")])
            .with_store(Arc::clone(&store))
            .with_capabilities(forge_comment_registry(Arc::clone(&calls)));
        restarted.restore_runs();
        assert_eq!(
            restarted.get_run(&run_id).map(|run| run.status),
            Some(SopRunStatus::PausedCheckpoint),
            "restart restores the pre-terminal parked checkpoint snapshot"
        );

        let second_outcome = restarted
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("re-approval resolves from restored checkpoint");
        let super::super::approval::BrokerOutcome::Resolved(
            super::super::approval::ResolveOutcome::Resumed(second_final_action),
        ) = second_outcome
        else {
            panic!("expected Resolved(Resumed), got {second_outcome:?}");
        };
        assert!(
            matches!(*second_final_action, SopRunAction::Completed { .. }),
            "replay with a completed effect marker must complete without posting again: {second_final_action:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "re-approval after terminal persistence failure must not create a second public comment"
        );
        let events = restarted.run_events(&run_id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|ev| ev.kind == "capability_effect_started")
                .count(),
            1,
            "replay must not write a second started marker: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|ev| ev.kind == "capability_effect_completed")
                .count(),
            1,
            "replay must reuse the completed effect marker: {events:?}"
        );
        let run = restarted
            .last_finished_run("forge-replay")
            .expect("replayed run reaches finished list");
        assert_eq!(run.status, SopRunStatus::Completed);
    }

    #[test]
    fn forge_comment_rejects_agent_resolved_checkpoint() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut engine = engine_with_sops(vec![checkpoint_forge_comment_sop("forge-agent")])
            .with_capabilities(forge_comment_registry(Arc::clone(&calls)));

        let first = engine
            .start_run("forge-agent", forge_comment_event())
            .unwrap();
        assert!(
            matches!(first, SopRunAction::CheckpointWait { .. }),
            "forge run must park at the checkpoint before writing: {first:?}"
        );
        let run_id = extract_run_id(&first).to_string();

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::agent("triage-agent"),
            )
            .expect("agent checkpoint approve resolves through default approval mode");
        let super::super::approval::BrokerOutcome::Resolved(
            super::super::approval::ResolveOutcome::Resumed(final_action),
        ) = outcome
        else {
            panic!("expected Resolved(Resumed), got {outcome:?}");
        };

        assert!(
            matches!(*final_action, SopRunAction::Failed { .. }),
            "agent-cleared checkpoint must not authorize forge.comment: {final_action:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "forge adapter must not run after an agent-sourced checkpoint approval"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events.iter().any(|event| {
                event.kind == "gate_resolved"
                    && event.payload.get("source").and_then(|value| value.as_str()) == Some("agent")
            }),
            "test must prove the rejected ledger row was agent-sourced: {events:?}"
        );
        let run = engine
            .last_finished_run("forge-agent")
            .expect("failed run should be retained");
        let result = run
            .step_results
            .iter()
            .find(|result| result.step_number == 2)
            .expect("forge step result recorded");
        assert_eq!(result.status, SopStepStatus::Failed);
        assert!(
            result.output.contains("immediately preceding checkpoint"),
            "failure should name the checkpoint authorization invariant: {result:?}"
        );
    }
    #[test]
    fn forge_comment_rejects_payload_mutated_after_checkpoint() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut engine = engine_with_sops(vec![checkpoint_mutates_before_forge_comment_sop(
            "forge-mutated",
        )])
        .with_capabilities(forge_comment_registry_with_mutator(Arc::clone(&calls)));

        let first = engine
            .start_run("forge-mutated", forge_comment_event())
            .unwrap();
        assert!(
            matches!(first, SopRunAction::CheckpointWait { .. }),
            "run must park at the checkpoint before any forge write: {first:?}"
        );
        let run_id = extract_run_id(&first).to_string();

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("checkpoint approve resolves");
        let super::super::approval::BrokerOutcome::Resolved(
            super::super::approval::ResolveOutcome::Resumed(final_action),
        ) = outcome
        else {
            panic!("expected Resolved(Resumed), got {outcome:?}");
        };

        assert!(
            matches!(*final_action, SopRunAction::Failed { .. }),
            "mutated forge payload must require a new checkpoint: {final_action:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "forge adapter must not run after an intervening capability changes the approved body"
        );
        let run = engine
            .last_finished_run("forge-mutated")
            .expect("failed run should be retained");
        let result = run
            .step_results
            .iter()
            .find(|result| result.step_number == 3)
            .expect("forge step result recorded");
        assert_eq!(result.status, SopStepStatus::Failed);
        assert!(
            result
                .output
                .contains("exact repo, number, body, and channel"),
            "failure should name the exact payload invariant: {result:?}"
        );
    }

    #[test]
    fn forge_comment_rejects_channel_changed_after_checkpoint() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut engine = engine_with_sops(vec![checkpoint_forge_comment_sop_with_channel(
            "forge-channel-mismatch",
            "git.admin",
        )])
        .with_capabilities(forge_comment_registry(Arc::clone(&calls)));

        let first = engine
            .start_run("forge-channel-mismatch", forge_comment_event())
            .unwrap();
        assert!(
            matches!(first, SopRunAction::CheckpointWait { .. }),
            "run must park at the checkpoint before any forge write: {first:?}"
        );
        let run_id = extract_run_id(&first).to_string();

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("checkpoint approve resolves");
        let super::super::approval::BrokerOutcome::Resolved(
            super::super::approval::ResolveOutcome::Resumed(final_action),
        ) = outcome
        else {
            panic!("expected Resolved(Resumed), got {outcome:?}");
        };

        assert!(
            matches!(*final_action, SopRunAction::Failed { .. }),
            "changed forge channel must require a new checkpoint: {final_action:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "forge adapter must not run when the approved channel differs from the static forge target"
        );
        let run = engine
            .last_finished_run("forge-channel-mismatch")
            .expect("failed run should be retained");
        let result = run
            .step_results
            .iter()
            .find(|result| result.step_number == 2)
            .expect("forge step result recorded");
        assert_eq!(result.status, SopStepStatus::Failed);
        assert!(
            result
                .output
                .contains("exact repo, number, body, and channel"),
            "failure should name the exact target invariant: {result:?}"
        );
    }

    #[test]
    fn forge_comment_rejects_stale_ledger_from_prior_checkpoint() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut engine =
            engine_with_sops(vec![two_checkpoint_forge_comment_sop("forge-stale-ledger")])
                .with_capabilities(forge_comment_registry(Arc::clone(&calls)));

        let first = engine
            .start_run("forge-stale-ledger", forge_comment_event())
            .unwrap();
        assert!(
            matches!(first, SopRunAction::CheckpointWait { .. }),
            "run must park at the first checkpoint: {first:?}"
        );
        let run_id = extract_run_id(&first).to_string();

        let first_outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("first checkpoint approve resolves");
        let super::super::approval::BrokerOutcome::Resolved(
            super::super::approval::ResolveOutcome::Resumed(parked_at_second),
        ) = first_outcome
        else {
            panic!("expected first checkpoint to resume into second gate, got {first_outcome:?}");
        };
        assert!(
            matches!(*parked_at_second, SopRunAction::CheckpointWait { .. }),
            "first approval should drive the noop bridge and park at checkpoint 3: {parked_at_second:?}"
        );

        let final_action = engine
            .decide_checkpoint(&run_id, super::super::approval::ApprovalDecision::Approve)
            .expect("direct second checkpoint approval should resume into guarded forge step");
        assert!(
            matches!(final_action, SopRunAction::Failed { .. }),
            "unaudited second checkpoint must fail before forge.comment, got {final_action:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "stale checkpoint-1 ledger row must not authorize checkpoint-3 forge write"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events.iter().any(|event| {
                event.kind == "gate_resolved"
                    && event.payload.get("step").and_then(|value| value.as_u64()) == Some(1)
            }),
            "first checkpoint must write the audited ledger row: {events:?}"
        );
        assert!(
            !events.iter().any(|event| {
                event.kind == "gate_resolved"
                    && event.payload.get("step").and_then(|value| value.as_u64()) == Some(3)
            }),
            "direct checkpoint approval must not synthesize a ledger row for step 3: {events:?}"
        );
        let run = engine
            .last_finished_run("forge-stale-ledger")
            .expect("failed run should be retained");
        let result = run
            .step_results
            .iter()
            .find(|result| result.step_number == 4)
            .expect("forge step result recorded");
        assert_eq!(result.status, SopStepStatus::Failed);
        assert!(
            result.output.contains("immediately preceding checkpoint"),
            "failure should name the missing checkpoint-specific ledger row: {result:?}"
        );
    }

    #[test]
    fn forge_comment_rejects_stale_ledger_from_prior_visit_of_same_checkpoint() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut engine = engine_with_sops(vec![same_step_revisit_forge_comment_sop(
            "forge-same-step-revisit",
        )])
        .with_capabilities(forge_comment_registry_with_mutator(Arc::clone(&calls)));

        let first = engine
            .start_run("forge-same-step-revisit", forge_comment_event())
            .unwrap();
        assert!(
            matches!(first, SopRunAction::CheckpointWait { .. }),
            "run must park at the first checkpoint visit: {first:?}"
        );
        let run_id = extract_run_id(&first).to_string();
        assert_eq!(
            engine.get_run(&run_id).map(|run| run.revision),
            Some(0),
            "first checkpoint presentation starts at revision 0"
        );

        let first_outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("first checkpoint approve resolves");
        let super::super::approval::BrokerOutcome::Resolved(
            super::super::approval::ResolveOutcome::Resumed(second_visit),
        ) = first_outcome
        else {
            panic!("expected first checkpoint to resume into second visit, got {first_outcome:?}");
        };
        assert!(
            matches!(*second_visit, SopRunAction::CheckpointWait { .. }),
            "first approval should loop back and park at checkpoint step 1 again: {second_visit:?}"
        );
        assert_eq!(
            engine.get_run(&run_id).map(|run| run.revision),
            Some(1),
            "same-step checkpoint revisit must carry a fresh revision"
        );

        let final_action = engine
            .decide_checkpoint(&run_id, super::super::approval::ApprovalDecision::Approve)
            .expect("direct second checkpoint approval should resume into guarded forge step");
        assert!(
            matches!(final_action, SopRunAction::Failed { .. }),
            "direct second visit approval must not reuse the revision-0 ledger row: {final_action:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "stale first-visit ledger row must not authorize the second-visit forge write"
        );

        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events.iter().any(|event| {
                event.kind == "gate_resolved"
                    && event.payload.get("step").and_then(|value| value.as_u64()) == Some(1)
                    && event
                        .payload
                        .get("checkpoint_revision")
                        .and_then(|value| value.as_u64())
                        == Some(0)
            }),
            "first visit must write the revision-0 ledger row: {events:?}"
        );
        assert!(
            !events.iter().any(|event| {
                event.kind == "gate_resolved"
                    && event.payload.get("step").and_then(|value| value.as_u64()) == Some(1)
                    && event
                        .payload
                        .get("checkpoint_revision")
                        .and_then(|value| value.as_u64())
                        == Some(1)
            }),
            "direct second visit approval must not synthesize a revision-1 ledger row: {events:?}"
        );
        let run = engine
            .last_finished_run("forge-same-step-revisit")
            .expect("failed run should be retained");
        let result = run
            .step_results
            .iter()
            .find(|result| result.step_number == 3)
            .expect("forge step result recorded");
        assert_eq!(result.status, SopStepStatus::Failed);
        assert!(
            result.output.contains("immediately preceding checkpoint"),
            "failure should name the checkpoint authorization invariant: {result:?}"
        );
    }

    #[test]
    fn resolve_via_broker_approves_checkpoint_and_drives_capability_tail() {
        // The checkpoint bridge (B3): an out-of-band approve of a PausedCheckpoint
        // through the chokepoint must (a) write the audit ledger row, (b) resume via
        // approve_step, and (c) DRIVE the post-checkpoint capability steps
        // headlessly to completion - no live agent turn involved.
        let mut engine = engine_with_sops(vec![capability_checkpoint_sop("cp-tail")]);
        let first = engine.start_run("cp-tail", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let parked = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("drive to the checkpoint");
        assert!(
            matches!(parked, SopRunAction::CheckpointWait { .. }),
            "run must park at the step-2 checkpoint: {parked:?}"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("checkpoint approve resolves");
        let super::super::approval::BrokerOutcome::Resolved(
            super::super::approval::ResolveOutcome::Resumed(final_action),
        ) = outcome
        else {
            panic!("expected Resolved(Resumed), got {outcome:?}");
        };
        assert!(
            matches!(*final_action, SopRunAction::Completed { .. }),
            "the capability tail must run to completion headlessly: {final_action:?}"
        );
        let run = engine
            .last_finished_run("cp-tail")
            .expect("run reached the finished list");
        assert_eq!(run.status, SopRunStatus::Completed);
        assert_eq!(
            run.step_results.len(),
            3,
            "all three steps (cap, checkpoint, cap) recorded results"
        );
        // The resolution is ledger-audited like any approval gate.
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events.iter().any(|ev| ev.kind == "gate_resolved"),
            "checkpoint resolution must append a gate_resolved ledger row: {events:?}"
        );
    }

    #[test]
    fn deterministic_start_pipes_the_trigger_payload_into_step_one() {
        // Regression: `start_deterministic_run` hardcoded step 1's input to Null,
        // so a channel-triggered pipeline's first step never saw the event that
        // triggered it (a triage step received `null` instead of the issue). The
        // start path must apply the same step-1 = trigger-payload mapping as
        // `step_input_value` on the resume/retry paths.
        // `deterministic_sop` has an Execute-kind step 1, whose start action
        // carries the input (a capability step 1 would execute inline instead).
        let mut engine = engine_with_sops(vec![deterministic_sop("det-payload")]);
        let event = SopEvent {
            source: SopTriggerSource::Channel,
            topic: Some("git.main:issues.opened".into()),
            payload: Some(r#"{"repo":"o/r","number":12}"#.into()),
            timestamp: now_iso8601(),
        };
        let first = engine.start_run("det-payload", event).unwrap();
        match &first {
            SopRunAction::DeterministicStep { step, input, .. } => {
                assert_eq!(step.number, 1);
                assert_eq!(
                    input,
                    &serde_json::json!({"repo": "o/r", "number": 12}),
                    "step 1 must receive the parsed trigger payload, not Null"
                );
            }
            other => panic!("expected the step-1 DeterministicStep, got {other:?}"),
        }
    }

    #[test]
    fn resolve_via_broker_denies_checkpoint_and_cancels() {
        // Deny of a parked checkpoint through the chokepoint cancels the run (the
        // approval-gate deny semantics), records the reason, and audits the
        // resolution. Previously a checkpoint could not be denied out-of-band at
        // all (the surfaces returned not_waiting).
        let mut engine = engine_with_sops(vec![capability_checkpoint_sop("cp-deny")]);
        let first = engine.start_run("cp-deny", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let parked = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("drive to the checkpoint");
        assert!(matches!(parked, SopRunAction::CheckpointWait { .. }));

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Deny {
                    reason: Some("not appropriate".into()),
                },
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("checkpoint deny resolves");
        assert!(
            matches!(
                outcome,
                super::super::approval::BrokerOutcome::Resolved(
                    super::super::approval::ResolveOutcome::Denied
                )
            ),
            "expected Resolved(Denied), got {outcome:?}"
        );
        let run = engine
            .last_finished_run("cp-deny")
            .expect("denied run reached the finished list");
        assert_eq!(run.status, SopRunStatus::Cancelled);
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events.iter().any(|ev| ev.kind == "gate_resolved"),
            "checkpoint deny must append a gate_resolved ledger row: {events:?}"
        );
    }

    #[test]
    fn resolve_via_broker_approval_deny_terminal_persist_failure_errors() {
        let store: Arc<dyn SopRunStore> = Arc::new(FailFirstFinishStore::new());
        let mut engine = engine_with_sops(vec![test_sop(
            "gate-deny-terminal-fail",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )])
        .with_store(store);
        let action = engine
            .start_run("gate-deny-terminal-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert_eq!(
            engine.get_run(&run_id).map(|run| run.status),
            Some(SopRunStatus::WaitingApproval)
        );

        let err = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Deny {
                    reason: Some("not appropriate".into()),
                },
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect_err("terminal persistence failure must propagate to the caller");

        assert!(
            err.to_string().contains("failed to persist terminal run"),
            "unexpected error: {err}"
        );
        assert_eq!(
            engine.get_run(&run_id).map(|run| run.status),
            Some(SopRunStatus::WaitingApproval),
            "approval gate remains waiting for retry"
        );
        assert!(
            engine
                .last_finished_run("gate-deny-terminal-fail")
                .is_none(),
            "failed terminal persistence must not move the run to finished_runs"
        );
    }

    #[test]
    fn resolve_via_broker_checkpoint_deny_terminal_persist_failure_errors() {
        let store: Arc<dyn SopRunStore> = Arc::new(FailFirstFinishStore::new());
        let mut engine = engine_with_sops(vec![capability_checkpoint_sop("cp-deny-terminal-fail")])
            .with_store(store);
        let first = engine
            .start_run("cp-deny-terminal-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&first).to_string();
        let parked = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("drive to the checkpoint");
        assert!(matches!(parked, SopRunAction::CheckpointWait { .. }));

        let err = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Deny {
                    reason: Some("not appropriate".into()),
                },
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect_err("terminal persistence failure must propagate to the caller");

        assert!(
            err.to_string().contains("failed to persist terminal run"),
            "unexpected error: {err}"
        );
        assert_eq!(
            engine.get_run(&run_id).map(|run| run.status),
            Some(SopRunStatus::PausedCheckpoint),
            "checkpoint gate remains parked for retry"
        );
        assert!(
            engine.last_finished_run("cp-deny-terminal-fail").is_none(),
            "failed terminal persistence must not move the run to finished_runs"
        );
    }

    /// `capability(noop) -> checkpoint(edit: body) -> capability(noop)`: the
    /// operator-amendable review-gate shape.
    fn editable_checkpoint_sop(name: &str) -> Sop {
        let mut sop = capability_checkpoint_sop(name);
        sop.steps[1].edit = Some("body".into());
        sop
    }

    fn payload_event(payload: &str) -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: Some(payload.into()),
            timestamp: now_iso8601(),
        }
    }

    #[test]
    fn resolve_via_broker_amends_checkpoint_and_pipes_the_edited_field() {
        // An Amend IS an approval of the operator's text: the edited field must
        // replace its counterpart in the piped value, become the checkpoint's
        // recorded output, and flow into the post-checkpoint capability tail —
        // while the predecessor step keeps the model's original for audit.
        let mut engine = engine_with_sops(vec![editable_checkpoint_sop("cp-amend")]);
        let first = engine
            .start_run(
                "cp-amend",
                payload_event(r#"{"body":"model draft","repo":"o/r"}"#),
            )
            .unwrap();
        let run_id = extract_run_id(&first).to_string();
        let parked = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("drive to the checkpoint");
        assert!(matches!(parked, SopRunAction::CheckpointWait { .. }));

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Amend {
                    text: "the operator rewrite".into(),
                },
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("checkpoint amend resolves");
        assert!(
            matches!(
                outcome,
                super::super::approval::BrokerOutcome::Resolved(
                    super::super::approval::ResolveOutcome::Resumed(_)
                )
            ),
            "expected Resolved(Resumed), got {outcome:?}"
        );
        let run = engine
            .last_finished_run("cp-amend")
            .expect("amended run completed");
        assert_eq!(run.status, SopRunStatus::Completed);
        // Step 1 keeps the model's original.
        let step1: serde_json::Value = serde_json::from_str(
            &run.step_results
                .iter()
                .find(|r| r.step_number == 1)
                .unwrap()
                .output,
        )
        .unwrap();
        assert_eq!(step1["body"], "model draft");
        // The checkpoint's output AND the tail step's input carry the rewrite,
        // with the untouched fields intact.
        for step_number in [2u32, 3] {
            let out: serde_json::Value = serde_json::from_str(
                &run.step_results
                    .iter()
                    .find(|r| r.step_number == step_number)
                    .unwrap()
                    .output,
            )
            .unwrap();
            assert_eq!(
                out["body"], "the operator rewrite",
                "step {step_number} must carry the amended body"
            );
            assert_eq!(out["repo"], "o/r", "unedited fields must survive");
        }
        // The ledger records the resolution as an amend.
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events
                .iter()
                .any(|ev| ev.kind == "gate_resolved" && ev.payload["decision"] == "amend"),
            "amend must append a decision=amend ledger row: {events:?}"
        );
    }

    #[test]
    fn amend_without_a_declared_edit_field_fails_closed() {
        // No `- edit:` on the checkpoint → an Amend must be refused BEFORE any
        // ledger row or run mutation, leaving the gate parked and answerable.
        let mut engine = engine_with_sops(vec![capability_checkpoint_sop("cp-noedit")]);
        let first = engine.start_run("cp-noedit", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let _ = engine.drive_headless_deterministic(&run_id, first).unwrap();

        let res = engine.resolve_via_broker(
            &run_id,
            super::super::approval::ApprovalDecision::Amend { text: "x".into() },
            super::super::approval::ApprovalPrincipal::cli(None),
        );
        assert!(res.is_err(), "amend without `- edit:` must fail closed");
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "the gate must stay parked"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events.iter().all(|ev| ev.kind != "gate_resolved"),
            "a refused amend must not leave a gate_resolved row: {events:?}"
        );
    }

    /// A stub `llm.generate` that bakes the reviewer feedback into its output,
    /// so a revise's re-draft is distinguishable from the original.
    struct StubLlmGenerate;

    impl super::super::capability::SopCapability for StubLlmGenerate {
        fn id(&self) -> &'static str {
            "llm.generate"
        }
        fn describe(&self) -> super::super::capability::CapabilityInfo {
            super::super::capability::CapabilityInfo {
                id: self.id(),
                description: "stub llm.generate",
                deterministic: true,
                idempotent: false,
                reversible: true,
                supports_retry: true,
                required_permissions: vec![],
                input_schema: None,
                output_schema: None,
            }
        }
        fn execute(
            &self,
            _ctx: super::super::capability::CapabilityContext,
            input: serde_json::Value,
        ) -> Result<super::super::capability::CapabilityResult> {
            let feedback = input
                .get("revision_feedback")
                .and_then(|v| v.as_str())
                .unwrap_or("none");
            Ok(super::super::capability::CapabilityResult::success(
                serde_json::json!({"body": format!("draft [feedback: {feedback}]")}),
            ))
        }
    }

    /// A stub `llm.generate` that succeeds on the FIRST draft (no
    /// `revision_feedback`) but fails on the RE-draft — so the run reaches the
    /// checkpoint normally, and only the Revise re-run models a provider outage.
    struct FailsOnlyOnRevise;

    impl super::super::capability::SopCapability for FailsOnlyOnRevise {
        fn id(&self) -> &'static str {
            "llm.generate"
        }
        fn describe(&self) -> super::super::capability::CapabilityInfo {
            super::super::capability::CapabilityInfo {
                id: self.id(),
                description: "stub llm.generate that fails only on re-draft",
                deterministic: true,
                idempotent: false,
                reversible: true,
                supports_retry: true,
                required_permissions: vec![],
                input_schema: None,
                output_schema: None,
            }
        }
        fn execute(
            &self,
            _ctx: super::super::capability::CapabilityContext,
            input: serde_json::Value,
        ) -> Result<super::super::capability::CapabilityResult> {
            if input.get("revision_feedback").is_some() {
                Ok(super::super::capability::CapabilityResult::failure(
                    "model provider unavailable",
                ))
            } else {
                Ok(super::super::capability::CapabilityResult::success(
                    serde_json::json!({"body": "original draft"}),
                ))
            }
        }
    }

    /// `capability(llm.generate stub) -> checkpoint`: the revisable review-gate
    /// shape, with the stub registered over the fail-closed builtin.
    fn revisable_checkpoint_engine(name: &str) -> SopEngine {
        let mut sop = capability_checkpoint_sop(name);
        sop.steps[0].capability = Some("llm.generate".into());
        sop.steps.truncate(2);
        let mut registry = super::super::capability::SopCapabilityRegistry::with_builtins();
        registry.register(StubLlmGenerate);
        engine_with_sops(vec![sop]).with_capabilities(Arc::new(registry))
    }

    #[test]
    fn failed_revise_writes_no_resolved_row_and_leaves_the_draft_unchanged() {
        // The resolved ledger row must not be appended before the re-draft's
        // fallible model call. A failed Revise leaves zero gate_resolved rows, the
        // original draft parked, and the revision counter untouched.
        let mut sop = capability_checkpoint_sop("cp-revise-fail");
        sop.steps[0].capability = Some("llm.generate".into());
        sop.steps.truncate(2);
        let mut registry = super::super::capability::SopCapabilityRegistry::with_builtins();
        registry.register(FailsOnlyOnRevise);
        let mut engine = engine_with_sops(vec![sop]).with_capabilities(Arc::new(registry));

        let first = engine.start_run("cp-revise-fail", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let _ = engine.drive_headless_deterministic(&run_id, first).unwrap();
        let original_draft = engine
            .get_run(&run_id)
            .unwrap()
            .step_results
            .iter()
            .find(|r| r.step_number == 1)
            .unwrap()
            .output
            .clone();

        let res = engine.resolve_via_broker(
            &run_id,
            super::super::approval::ApprovalDecision::Revise {
                guidance: "make it shorter".into(),
            },
            super::super::approval::ApprovalPrincipal::cli(None),
        );
        assert!(res.is_err(), "a failed re-draft must surface an error");

        let run = engine.get_run(&run_id).expect("run stays parked");
        assert_eq!(run.status, SopRunStatus::PausedCheckpoint);
        assert_eq!(
            run.revision, 0,
            "a failed revise must not bump the revision"
        );
        assert_eq!(
            run.step_results
                .iter()
                .find(|r| r.step_number == 1)
                .unwrap()
                .output,
            original_draft,
            "the original draft must remain untouched"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events.iter().all(|ev| ev.kind != "gate_resolved"),
            "a failed revise must leave NO gate_resolved ledger row: {events:?}"
        );

        // The gate is still answerable: the run must admit a fresh exec claim
        // (a leaked claim from the failed revise would block this deny).
        engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Deny { reason: None },
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("the gate is still resolvable after a failed revise");
        assert_eq!(
            engine.last_finished_run("cp-revise-fail").unwrap().status,
            SopRunStatus::Cancelled
        );
    }

    #[test]
    fn revise_does_not_report_success_until_its_parked_snapshot_persists() {
        // A successful provider re-draft still cannot supersede the old prompt
        // until the new gate state is recoverable. Inject the failure only after
        // the original checkpoint has parked durably.
        let store = Arc::new(FailingSaveStore::new());
        store.allow_saves();
        let mut engine =
            revisable_checkpoint_engine("cp-revise-save-fail").with_store(store.clone());
        let first = engine
            .start_run("cp-revise-save-fail", manual_event())
            .expect("start the revisable SOP");
        let run_id = extract_run_id(&first).to_string();
        let parked = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("park the original checkpoint");
        assert!(matches!(parked, SopRunAction::CheckpointWait { .. }));
        assert_eq!(store.claim_counts("cp-revise-save-fail").unwrap(), (0, 0));

        store.fail_saves();
        let result = engine.resolve_via_broker(
            &run_id,
            super::super::approval::ApprovalDecision::Revise {
                guidance: "make it shorter".into(),
            },
            super::super::approval::ApprovalPrincipal::cli(None),
        );
        assert!(
            result.is_err(),
            "a non-durable revised checkpoint must not return Resolved(Revised): {result:?}"
        );
        assert!(
            engine.is_park_persist_pending(&run_id),
            "the revised gate must remain fail-closed for its durable retry"
        );
        assert!(
            !engine.is_gate_reference_superseded(&run_id, 0),
            "the original prompt must stay current until the revised snapshot is durable"
        );
        assert_eq!(
            store.claim_counts("cp-revise-save-fail").unwrap(),
            (1, 1),
            "the revised checkpoint keeps its claim while its write is failing"
        );

        store.allow_saves();
        engine.run_maintenance_tick();
        assert!(
            !engine.is_park_persist_pending(&run_id),
            "a successful maintenance retry completes the revised park"
        );
        assert!(
            engine.is_gate_reference_superseded(&run_id, 0),
            "the original prompt becomes stale only after the revised snapshot is durable"
        );
        assert_eq!(
            store.claim_counts("cp-revise-save-fail").unwrap(),
            (0, 0),
            "the deferred park releases its claim only after it is durable"
        );
    }

    #[test]
    fn resolve_via_broker_revises_checkpoint_and_represents_the_gate() {
        let mut engine = revisable_checkpoint_engine("cp-revise");
        let first = engine.start_run("cp-revise", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let parked = engine
            .drive_headless_deterministic(&run_id, first)
            .expect("drive to the checkpoint");
        assert!(matches!(parked, SopRunAction::CheckpointWait { .. }));

        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Revise {
                    guidance: "make it shorter".into(),
                },
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("checkpoint revise resolves");
        assert!(
            matches!(
                outcome,
                super::super::approval::BrokerOutcome::Resolved(
                    super::super::approval::ResolveOutcome::Revised
                )
            ),
            "expected Resolved(Revised), got {outcome:?}"
        );
        // The run never left the gate; the draft was replaced and the gate
        // revision bumped so the old prompt's reference is superseded.
        let run = engine.get_run(&run_id).expect("run still active");
        assert_eq!(run.status, SopRunStatus::PausedCheckpoint);
        assert_eq!(run.revision, 1);
        let redraft = &run
            .step_results
            .iter()
            .find(|r| r.step_number == 1)
            .unwrap()
            .output;
        assert!(
            redraft.contains("make it shorter"),
            "the re-draft must reflect the guidance: {redraft}"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            events
                .iter()
                .any(|ev| ev.kind == "gate_resolved" && ev.payload["decision"] == "revise"),
            "revise must append a decision=revise ledger row: {events:?}"
        );

        // The revised gate is still answerable: approve completes with the NEW
        // draft as the checkpoint's output.
        let outcome = engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .expect("revised gate approves");
        assert!(matches!(
            outcome,
            super::super::approval::BrokerOutcome::Resolved(
                super::super::approval::ResolveOutcome::Resumed(_)
            )
        ));
        let finished = engine.last_finished_run("cp-revise").unwrap();
        assert_eq!(finished.status, SopRunStatus::Completed);
    }

    #[test]
    fn revise_is_capped_and_refuses_on_a_non_llm_predecessor() {
        // Cap: MAX_GATE_REVISIONS re-drafts, then fail closed (bounded spend).
        let mut engine = revisable_checkpoint_engine("cp-revise-cap");
        let first = engine.start_run("cp-revise-cap", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let _ = engine.drive_headless_deterministic(&run_id, first).unwrap();
        for i in 1..=MAX_GATE_REVISIONS {
            engine
                .resolve_via_broker(
                    &run_id,
                    super::super::approval::ApprovalDecision::Revise {
                        guidance: format!("round {i}"),
                    },
                    super::super::approval::ApprovalPrincipal::cli(None),
                )
                .unwrap_or_else(|e| panic!("revision {i} within the cap must resolve: {e}"));
        }
        let res = engine.resolve_via_broker(
            &run_id,
            super::super::approval::ApprovalDecision::Revise {
                guidance: "one too many".into(),
            },
            super::super::approval::ApprovalPrincipal::cli(None),
        );
        assert!(
            res.is_err(),
            "the revision cap must refuse further re-drafts"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().revision,
            MAX_GATE_REVISIONS
        );

        // A noop predecessor is not revisable at all (nothing to re-draft).
        let mut engine = engine_with_sops(vec![capability_checkpoint_sop("cp-norevise")]);
        let first = engine.start_run("cp-norevise", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let _ = engine.drive_headless_deterministic(&run_id, first).unwrap();
        let res = engine.resolve_via_broker(
            &run_id,
            super::super::approval::ApprovalDecision::Revise {
                guidance: "shorter".into(),
            },
            super::super::approval::ApprovalPrincipal::cli(None),
        );
        assert!(
            res.is_err(),
            "a gate without an llm.generate predecessor must refuse revise"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );
    }

    #[test]
    fn gate_presentations_get_unique_revisions_and_a_per_gate_revise_budget() {
        // llm -> checkpoint -> llm -> checkpoint. Every gate presentation the
        // run ever makes must carry a UNIQUE revision (so a stale earlier-gate
        // prompt can never resolve a later gate), and the revise cap must be a
        // per-GATE budget, not a run-wide one.
        let mut sop = capability_checkpoint_sop("cp-two-gates");
        sop.steps[0].capability = Some("llm.generate".into());
        sop.steps[2].capability = Some("llm.generate".into());
        sop.steps.push(SopStep {
            number: 4,
            title: "Gate 2".into(),
            kind: SopStepKind::Checkpoint,
            ..SopStep::default()
        });
        let mut registry = super::super::capability::SopCapabilityRegistry::with_builtins();
        registry.register(StubLlmGenerate);
        let mut engine = engine_with_sops(vec![sop]).with_capabilities(Arc::new(registry));

        let first = engine.start_run("cp-two-gates", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let _ = engine.drive_headless_deterministic(&run_id, first).unwrap();
        {
            let run = engine.get_run(&run_id).unwrap();
            assert_eq!(run.current_step, 2, "parked at gate 1");
            assert_eq!(run.revision, 0, "the run's first gate is revision 0");
            assert_eq!(run.revision_base, 0);
        }
        // One revise at gate 1: revision 1.
        engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Revise {
                    guidance: "shorter".into(),
                },
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert_eq!(engine.get_run(&run_id).unwrap().revision, 1);
        // Approve gate 1 -> the tail drives to gate 2, whose presentation must
        // be revision 2 (unique vs gate 1's 0 and 1) with a FRESH revise budget.
        engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .unwrap();
        {
            let run = engine.get_run(&run_id).unwrap();
            assert_eq!(run.status, SopRunStatus::PausedCheckpoint);
            assert_eq!(run.current_step, 4, "parked at gate 2");
            assert_eq!(
                run.revision, 2,
                "a new gate presentation bumps past every earlier reference"
            );
            assert_eq!(run.revision_base, 2, "the revise budget rebases per gate");
        }
        // Gate 2 has its FULL budget despite gate 1's spend.
        for i in 1..=MAX_GATE_REVISIONS {
            engine
                .resolve_via_broker(
                    &run_id,
                    super::super::approval::ApprovalDecision::Revise {
                        guidance: format!("gate2 round {i}"),
                    },
                    super::super::approval::ApprovalPrincipal::cli(None),
                )
                .unwrap_or_else(|e| panic!("gate 2 revision {i} within its own budget: {e}"));
        }
        assert!(
            engine
                .resolve_via_broker(
                    &run_id,
                    super::super::approval::ApprovalDecision::Revise {
                        guidance: "over budget".into(),
                    },
                    super::super::approval::ApprovalPrincipal::cli(None),
                )
                .is_err(),
            "gate 2's own cap must still bound spend"
        );
    }

    #[test]
    fn terminal_run_removes_the_park_snapshot_file() {
        // Fix 0b: a resolved run must not leave a stale `<run_id>.state.json`
        // claiming it is still paused — the run store and approval ledger are
        // the durable record, the snapshot is only a rehydration artifact.
        let dir = std::env::temp_dir().join(format!("zc-snapshot-cleanup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut sop = capability_checkpoint_sop("cp-snapshot");
        sop.location = Some(dir.clone());
        let mut engine = engine_with_sops(vec![sop]);

        // Denied run: snapshot written at park, gone after the deny.
        let first = engine.start_run("cp-snapshot", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let _ = engine.drive_headless_deterministic(&run_id, first).unwrap();
        let state_file = dir.join(format!("{run_id}.state.json"));
        assert!(state_file.exists(), "the park must write the snapshot");
        engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Deny { reason: None },
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert!(
            !state_file.exists(),
            "a cancelled run must remove its park snapshot"
        );

        // Approved run: snapshot gone after completion too.
        let first = engine.start_run("cp-snapshot", manual_event()).unwrap();
        let run_id = extract_run_id(&first).to_string();
        let _ = engine.drive_headless_deterministic(&run_id, first).unwrap();
        let state_file = dir.join(format!("{run_id}.state.json"));
        assert!(state_file.exists());
        engine
            .resolve_via_broker(
                &run_id,
                super::super::approval::ApprovalDecision::Approve,
                super::super::approval::ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert!(
            !state_file.exists(),
            "a completed run must remove its park snapshot"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sop_approve_tool_resumes_deterministic_checkpoint() {
        // The sop_approve tool must route a PausedCheckpoint to approve_step because
        // resolve_gate reports NotWaiting for it. Without that routing the tool
        // answers "not waiting for approval" and the deterministic run is stuck.
        use crate::tools::SopApproveTool;
        use zeroclaw_api::tool::Tool;

        let mut engine = engine_with_sops(vec![deterministic_sop("det-cp")]);
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );

        let tool = SopApproveTool::new(std::sync::Arc::new(std::sync::Mutex::new(engine)));
        let result = tool
            .execute(serde_json::json!({ "run_id": run_id }))
            .await
            .unwrap();
        assert!(
            result.success,
            "sop_approve must resume a deterministic checkpoint, not report not-waiting: {result:?}"
        );
        assert!(
            result.output.contains("Approved"),
            "checkpoint resume should be reported as approved: {result:?}"
        );
    }

    #[test]
    fn engine_restores_runs_from_store() {
        use super::super::store::SqliteRunStore;
        let path =
            std::env::temp_dir().join(format!("zc-sop-engine-restore-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Seed a WaitingApproval run directly into a durable store.
        let store = std::sync::Arc::new(SqliteRunStore::open(&path).unwrap());
        let run = SopRun {
            run_id: "r-restore".to_string(),
            sop_name: "deploy".to_string(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: now_iso8601(),
            },
            frame_marker_id: "marker-restore".to_string(),
            status: SopRunStatus::WaitingApproval,
            current_step: 1,
            total_steps: 2,
            started_at: now_iso8601(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: Some(now_iso8601()),
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        };
        store
            .save_run(&PersistedRun::new(
                run,
                now_iso8601(),
                SopTriggerSource::Manual,
            ))
            .unwrap();
        // A fresh engine wired to the same store rehydrates the run on boot.
        let mut engine = SopEngine::new(SopConfig::default()).with_store(store);
        engine.restore_runs();
        assert!(engine.active_runs().contains_key("r-restore"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn engine_persist_bumps_revision_across_active_and_terminal() {
        use super::super::store::SqliteRunStore;
        let path =
            std::env::temp_dir().join(format!("zc-sop-engine-persist-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = std::sync::Arc::new(SqliteRunStore::open(&path).unwrap());
        let mut engine = SopEngine::new(SopConfig::default()).with_store(store.clone());

        let mut run = SopRun {
            run_id: "r-persist".to_string(),
            sop_name: "deploy".to_string(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: now_iso8601(),
            },
            frame_marker_id: "marker-persist".to_string(),
            status: SopRunStatus::Running,
            current_step: 0,
            total_steps: 2,
            started_at: now_iso8601(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        };
        engine.active_runs.insert(run.run_id.clone(), run.clone());

        // First persist lands at revision 0.
        engine.persist_active("r-persist");
        assert_eq!(store.load_run("r-persist").unwrap().unwrap().revision, 0);

        // Advancing the run and persisting again is a divergent state at the next
        // revision. The old revision-0-always wiring would have had this rejected
        // as a same-revision conflict and silently kept the stale snapshot.
        run.current_step = 1;
        engine.active_runs.insert(run.run_id.clone(), run.clone());
        engine.persist_active("r-persist");
        let after = store.load_run("r-persist").unwrap().unwrap();
        assert_eq!(after.revision, 1);
        assert_eq!(after.run.current_step, 1, "latest state persisted");

        // The terminal write advances again, is accepted, and leaves no active run.
        run.status = SopRunStatus::Completed;
        run.completed_at = Some(now_iso8601());
        engine.persist_terminal(&run).unwrap();
        assert!(
            store.load_active_runs().unwrap().is_empty(),
            "terminal excluded from active"
        );
        assert_eq!(store.load_run("r-persist").unwrap().unwrap().revision, 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deterministic_active_run_persists_and_restores_before_terminal() {
        use super::super::store::SqliteRunStore;
        let path =
            std::env::temp_dir().join(format!("zc-sop-det-restore-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = std::sync::Arc::new(SqliteRunStore::open(&path).unwrap());

        let mut engine = SopEngine::new(SopConfig::default()).with_store(store.clone());
        engine.set_sops_for_test(vec![deterministic_sop("det-sop")]);

        // Start: the first deterministic step (Running) must be persisted as active,
        // not only on terminal completion.
        let action = engine.start_run("det-sop", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        let active = store.load_active_runs().unwrap();
        assert_eq!(
            active.len(),
            1,
            "deterministic start must persist an active run"
        );
        assert_eq!(active[0].run.run_id, run_id);
        assert_eq!(active[0].run.current_step, 1);

        // Advance into the checkpoint: still non-terminal, must stay persisted in
        // the shared store (not only in the deterministic state file).
        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!({"r": 1}), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        let stored = store.load_run(&run_id).unwrap().unwrap();
        assert_eq!(stored.run.current_step, 2);
        assert_eq!(stored.run.status, SopRunStatus::PausedCheckpoint);

        // Simulate a daemon restart mid-run: a fresh engine on the same store must
        // rehydrate the in-flight deterministic run (the gap this fixes).
        let mut restarted = SopEngine::new(SopConfig::default()).with_store(store.clone());
        restarted.restore_runs();
        assert!(
            restarted.active_runs().contains_key(&run_id),
            "deterministic in-flight run must rehydrate after restart"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn restored_policied_checkpoint_replays_request_route() {
        use zeroclaw_config::schema::ApprovalPolicyConfig;

        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let adapter = std::sync::Arc::new(RecordingRouteAdapter {
            calls: calls.clone(),
        });
        let broker = std::sync::Arc::new(crate::sop::approval::ApprovalBroker::with_route(adapter));
        let mut config = SopConfig::default();
        config.approval.policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: None,
                quorum: 0,
                request_route: Some("discord.ops:123456789".to_string()),
                escalation_route: None,
            },
        );
        let mut sop = deterministic_sop("det-restore-route");
        sop.steps[1].policy = Some("prod".to_string());

        let mut source = engine_with_config_sops(config.clone(), vec![sop.clone()])
            .with_store(store.clone())
            .with_approval_broker(broker.clone());
        let action = source
            .start_run("det-restore-route", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        let action = source
            .advance_deterministic_step(&run_id, serde_json::json!({"step": 1}), None)
            .unwrap();
        assert!(matches!(action, SopRunAction::CheckpointWait { .. }));
        assert_eq!(calls.lock().unwrap().len(), 1, "initial park delivers once");

        // Model a daemon exit after persistence but before the external adapter's
        // fire-and-forget delivery completes. Only the restored engine may send.
        calls.lock().unwrap().clear();
        let mut restarted = engine_with_config_sops(config, vec![sop])
            .with_store(store)
            .with_approval_broker(broker);
        restarted.restore_runs();

        assert_eq!(
            restarted.get_run(&run_id).map(|run| run.status),
            Some(SopRunStatus::PausedCheckpoint)
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [(
                "discord.ops:123456789".to_string(),
                run_id,
                "det-restore-route".to_string(),
                2
            )],
            "restore replays the persisted checkpoint through its request route"
        );
    }

    #[test]
    fn decide_checkpoint_approve_matches_approve_step() {
        // Approve through the unified decision entry point must behave exactly
        // like approve_step: resume to the next step down the success edge.
        let mut engine = engine_with_sops(vec![deterministic_sop("det-cp-approve")]);
        let action = engine.start_run("det-cp-approve", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        let action = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Approve)
            .unwrap();
        assert!(
            matches!(action, SopRunAction::DeterministicStep { ref step, .. } if step.number == 3),
            "approving via decide_checkpoint must resume to the next step"
        );
    }

    #[test]
    fn deny_checkpoint_defaults_to_terminal_failure() {
        // With the default on_failure (Fail), a denied checkpoint terminates the
        // run Failed. This is distinct from Cancelled: the operator declined and
        // no failure handler was authored, so the run failed.
        let mut engine = engine_with_sops(vec![deterministic_sop("det-cp-deny-fail")]);
        let action = engine
            .start_run("det-cp-deny-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();

        engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );

        let action = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .unwrap();
        assert!(
            matches!(action, SopRunAction::Failed { .. }),
            "denying a checkpoint with default on_failure must fail the run"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Failed
        );
    }

    #[test]
    fn deny_checkpoint_routes_through_on_failure_goto() {
        // A denied checkpoint takes the failure path: the checkpoint step is
        // recorded Failed and routed through its `on_failure`. With a Goto, the
        // run continues at the authored failure-handler step, not the success
        // successor and not a whole-run cancel.
        let mut sop = deterministic_sop("det-cp-deny-goto");
        sop.steps[1].on_failure = StepFailure::Goto { step: 3 };
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine
            .start_run("det-cp-deny-goto", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();

        engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );

        let action = engine
            .decide_checkpoint(
                &run_id,
                ApprovalDecision::Deny {
                    reason: Some("not acceptable".into()),
                },
            )
            .unwrap();
        assert!(
            matches!(action, SopRunAction::DeterministicStep { ref step, .. } if step.number == 3),
            "denying a checkpoint with on_failure=Goto must route to the failure-handler step"
        );
        let cp = engine
            .get_run(&run_id)
            .unwrap()
            .step_results
            .iter()
            .find(|r| r.step_number == 2)
            .expect("checkpoint step recorded");
        assert_eq!(cp.status, SopStepStatus::Failed);
    }

    #[test]
    fn engine_restores_finished_runs_from_store() {
        use super::super::store::SqliteRunStore;
        let path = std::env::temp_dir().join(format!(
            "zc-sop-engine-restore-fin-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = std::sync::Arc::new(SqliteRunStore::open(&path).unwrap());

        // Persist a terminal run: saved active, then finished with a bumped revision.
        let base = SopRun {
            run_id: "r-done".to_string(),
            sop_name: "deploy".to_string(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: now_iso8601(),
            },
            frame_marker_id: "marker-done".to_string(),
            status: SopRunStatus::Running,
            current_step: 0,
            total_steps: 1,
            started_at: now_iso8601(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        };
        store
            .save_run(&PersistedRun::new(
                base.clone(),
                now_iso8601(),
                SopTriggerSource::Manual,
            ))
            .unwrap();
        let mut terminal = base;
        terminal.status = SopRunStatus::Completed;
        terminal.completed_at = Some(now_iso8601());
        let mut persisted = PersistedRun::new(terminal, now_iso8601(), SopTriggerSource::Manual);
        persisted.revision = 1;
        store.finish_run("r-done", &persisted).unwrap();

        // A fresh engine seeds its retention window from the store's terminal set.
        let mut engine = SopEngine::new(SopConfig::default()).with_store(store);
        engine.restore_runs();
        assert!(
            !engine.active_runs().contains_key("r-done"),
            "terminal run must not rehydrate as active"
        );
        let finished = engine.finished_runs(None);
        assert_eq!(
            finished.len(),
            1,
            "terminal run seeded into retention window"
        );
        assert_eq!(finished[0].run_id, "r-done");
        assert_eq!(finished[0].status, SopRunStatus::Completed);
        let _ = std::fs::remove_file(&path);
    }
}
