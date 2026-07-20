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
    /// Run IDs parked at a checkpoint whose denial tried to take the terminal
    /// path, but the terminal write failed after the run's exec claim was
    /// reacquired. The parked snapshot is already durable, so this set only
    /// renews the retained claim during maintenance; it must not release the
    /// claim until the operator retries to a durable outcome.
    claims_retained_after_terminal_rollback: std::collections::HashSet<String>,
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

#[derive(Debug)]
struct TerminalPersistenceRetained {
    run_id: String,
    source: StoreError,
}

impl std::fmt::Display for TerminalPersistenceRetained {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "terminal persistence failed for run {}; active run and admission claim remain retained: {}",
            self.run_id, self.source
        )
    }
}

impl std::error::Error for TerminalPersistenceRetained {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Typed marker: a resume could not re-acquire an exec slot because the SOP's
/// per-SOP `max_concurrent` or the global `max_concurrent_total` is already
/// saturated. This is routine BACKPRESSURE, not a fault - kept distinct from a
/// store error so callers surface it as "at capacity, retry" (leaving the run
/// parked and re-resolvable) instead of logging it as a failure. It is the
/// signal that enforces the documented concurrency caps on the resume path: a
/// resume that would exceed them is refused rather than oversubscribed.
#[derive(Debug)]
struct ResumeAtCapacity {
    run_id: String,
    sop_name: String,
}

impl std::fmt::Display for ResumeAtCapacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "run {} ({}) cannot resume yet: execution slots are full; it stays parked and re-resolvable once a slot frees",
            self.run_id, self.sop_name
        )
    }
}

impl std::error::Error for ResumeAtCapacity {}

/// True when `err` is the typed [`ResumeAtCapacity`] backpressure marker (an
/// over-cap resume was refused), as opposed to a store fault. Lets a caller in
/// another module or crate (e.g. `resolve_gate`, or the gateway resume endpoint)
/// render it as backpressure (HTTP 503) rather than a fault without depending on
/// the private struct.
pub fn err_is_resume_at_capacity(err: &anyhow::Error) -> bool {
    err.is::<ResumeAtCapacity>()
}

enum ActivePersistOutcome {
    Saved,
    CapacityFull,
    Failed,
}

enum ParkPersistOutcome {
    Released,
    CapacityFull,
    PersistFailed,
}

enum GateClearTransition {
    Active {
        // Boxed: `SopRunAction` is large; keeping it inline makes this the
        // dominant variant (clippy::large_enum_variant).
        action: Box<SopRunAction>,
        follow_up: Option<GateResolutionFollowUp>,
    },
    Terminal {
        status: SopRunStatus,
        reason: Option<String>,
        follow_up: Option<GateResolutionFollowUp>,
    },
}

enum GateResolutionFollowUp {
    StepSchemaReject {
        step: u32,
        phase: &'static str,
        reason: String,
    },
    StepSkipped {
        sop_name: String,
        step: u32,
        reason: String,
    },
}

/// A held execution-slot reservation from phase 1 of a start (`reserve_run_slot`),
/// awaiting phase 2 (`activate_reserved_run`) or release (`release_reservation`).
/// Carries the CAS claim that keeps the slot held so the AMQP multi-match batch path
/// can reserve every matched SOP before activating any of them.
pub(crate) struct StartReservation {
    run_id: String,
    claim: ClaimToken,
    sop: Sop,
    deterministic: bool,
}

impl StartReservation {
    /// The SOP this reservation holds a slot for.
    pub(crate) fn sop_name(&self) -> &str {
        &self.sop.name
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
            claims_retained_after_terminal_rollback: std::collections::HashSet::new(),
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
    /// Reconstruct in-flight runs from the store at startup (durable backends).
    /// No-op for the in-memory default. Does not overwrite already-present runs.
    pub fn restore_runs(&mut self) {
        match self.store.load_active_runs() {
            Ok(runs) => {
                let mut restored = 0usize;
                for pr in runs {
                    // A1: a run persisted while parked at a HITL approval / paused at
                    // a deterministic checkpoint normally holds NO exec claim - it
                    // released its slot on park. Restore it WITHOUT re-establishing a
                    // claim unless the live claim is explicitly marked as retained
                    // after a failed terminal checkpoint decision.
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
                        let retained = match self
                            .store
                            .has_retained_terminal_rollback_claim(&pr.run.run_id)
                        {
                            Ok(retained) => retained,
                            Err(e) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "run_id": pr.run.run_id.as_str(),
                                            "error": e.to_string(),
                                        })
                                    ),
                                    "SOP engine: failed to inspect parked claim retention marker; failing closed (assuming retained)"
                                );
                                // FAIL CLOSED: a transient inspection read error must NOT
                                // discard a claim the terminal-rollback marker may exist to
                                // preserve (mapping it to `false` here would route into the
                                // release branch and drop that claim). Assume retained: the
                                // run keeps its claim. `heartbeat_claim` is an UPDATE-only
                                // no-op when the claim row is in fact already gone, so this
                                // cannot resurrect a released claim; the lease reaper reclaims
                                // a genuine orphan later. Erring toward keeping is the safe
                                // direction - releasing here could strand a run a real failed
                                // terminal write left restorable.
                                true
                            }
                        };
                        if retained && Self::terminal_rollback_marker_is_stale(&pr.run) {
                            // Crash-window reconcile: a terminal-rollback retention
                            // marker is legitimate ONLY when a genuine TERMINAL write
                            // failed and left the run restorable in its PRE-terminal
                            // parked state — i.e. still awaiting the (retried) decision
                            // at its current checkpoint, with NO recorded result for that
                            // step. A marker on a run that ALREADY recorded a terminal
                            // result for its current step reached this parked gate through
                            // a COMPLETED failure-route continuation (e.g. a denied
                            // checkpoint that Retried and re-parked). Its marker is stale —
                            // release it now rather than renew it forever.
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({
                                    "run_id": pr.run.run_id.as_str(),
                                    "current_step": pr.run.current_step,
                                })),
                                "SOP engine: releasing stale terminal-rollback claim on a continued parked run"
                            );
                            self.release_claim_best_effort(&Self::claim_handle_for_run(&pr.run));
                        } else if retained {
                            self.claims_retained_after_terminal_rollback
                                .insert(pr.run.run_id.clone());
                            self.heartbeat_claim_for_run(&pr.run);
                        } else {
                            // A parked run normally holds no exec slot. A durable store
                            // written by OLD behavior can carry a stale `sop_claims` row
                            // for this run; RELEASE it now so the restored parked run is
                            // genuinely claim-less and does not block admission.
                            self.release_claim_best_effort(&Self::claim_handle_for_run(&pr.run));
                        }
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
                    if self
                        .active_runs
                        .insert(pr.run.run_id.clone(), pr.run)
                        .is_none()
                    {
                        restored += 1;
                    }
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
        matches!(
            self.persist_active_checked_with_capacity(run_id, None),
            ActivePersistOutcome::Saved
        )
    }

    fn persist_active_checked_with_capacity(
        &self,
        run_id: &str,
        max_pending: Option<usize>,
    ) -> ActivePersistOutcome {
        let Some(run) = self.active_runs.get(run_id) else {
            return ActivePersistOutcome::Saved;
        };
        self.heartbeat_claim_for_run(run);
        let mut pr = PersistedRun::new(run.clone(), now_iso8601(), run.trigger_event.source);
        // Each persist is a new state revision; the store rejects a
        // same-revision divergent write, so advance past what is stored.
        pr.revision = self.next_run_revision(run_id);
        let outcome = match max_pending {
            Some(max_pending) => {
                match self.store.save_run_with_pending_capacity(&pr, max_pending) {
                    Ok(true) => ActivePersistOutcome::Saved,
                    Ok(false) => ActivePersistOutcome::CapacityFull,
                    Err(e) => {
                        Self::log_persist_failure(run_id, e);
                        ActivePersistOutcome::Failed
                    }
                }
            }
            None => match self.store.save_run(&pr) {
                Ok(()) => ActivePersistOutcome::Saved,
                Err(e) => {
                    Self::log_persist_failure(run_id, e);
                    ActivePersistOutcome::Failed
                }
            },
        };
        if !matches!(outcome, ActivePersistOutcome::CapacityFull) {
            self.notify_run(run, true);
        }
        outcome
    }

    fn log_persist_failure(run_id: &str, e: crate::sop::store::StoreError) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"run_id": run_id, "error": e.to_string()})),
            "SOP engine: failed to persist run"
        );
    }

    fn pending_capacity_limit_for_run(&self, run_id: &str) -> Option<usize> {
        let run = self.active_runs.get(run_id)?;
        let sop = self.sops.iter().find(|sop| sop.name == run.sop_name)?;
        (sop.max_pending_approvals > 0).then_some(sop.max_pending_approvals as usize)
    }

    fn pending_pool_full_reason(&self, sop: &Sop) -> Option<String> {
        if sop.max_pending_approvals == 0 {
            return None;
        }
        let pending = self.pending_count_for_sop(&sop.name);
        if pending >= sop.max_pending_approvals as usize {
            Some(format!(
                "SOP '{}' pending-approval pool full ({pending}/{})",
                sop.name, sop.max_pending_approvals
            ))
        } else {
            None
        }
    }

    fn pending_pool_capacity_raced_reason(&self, sop: &Sop) -> String {
        let pending = self.pending_count_for_sop(&sop.name);
        format!(
            "SOP '{}' pending-approval pool full ({pending}/{})",
            sop.name, sop.max_pending_approvals
        )
    }

    fn log_pending_capacity_full(run_id: &str, reason: &str) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"run_id": run_id, "reason": reason})),
            "SOP engine: pending-approval pool full at park transition; KEEPING the exec claim"
        );
    }
    fn persisted_active_snapshot(&self, run_id: &str) -> Result<(PersistedRun, SopRun)> {
        let run = self
            .active_runs
            .get(run_id)
            .cloned()
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        self.heartbeat_claim_for_run(&run);
        let mut persisted = PersistedRun::new(run.clone(), now_iso8601(), run.trigger_event.source);
        persisted.revision = self.next_run_revision(run_id);
        Ok((persisted, run))
    }

    /// Persist an active run transition and append its gate event as one store
    /// outcome. Used by `resolve_gate` so the durable gate ledger cannot get ahead
    /// of the run state transition it authorizes.
    pub(crate) fn persist_active_with_gate_event(
        &self,
        run_id: &str,
        event: &SopEventRecord,
    ) -> Result<()> {
        let (persisted, run) = self.persisted_active_snapshot(run_id)?;
        self.store.save_run_with_event(&persisted, event).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(
                        ::serde_json::json!({"run_id": run_id, "error": e.to_string()})
                    ),
                "SOP engine: gate resolution persistence failed; run transition and ledger remain uncommitted"
            );
            anyhow::Error::new(e)
        })?;
        self.notify_run(&run, true);
        Ok(())
    }

    /// Park a run (WaitingApproval / PausedCheckpoint) and free its exec slot, but
    /// ONLY after the parked snapshot is durably persisted. If the persist fails,
    /// the claim is KEPT (fail closed): the run stays correctly counted against
    /// capacity, so it is never both claimless AND un-persisted (which a crash
    /// would turn into a lost park while newer triggers had already admitted into
    /// the "freed" slot). The slot is held until a later persist succeeds,
    /// trading a little concurrency for no lost park.
    fn persist_parked_snapshot_then_release_claim(&mut self, run_id: &str) -> ParkPersistOutcome {
        let max_pending = self.pending_capacity_limit_for_run(run_id);
        match self.persist_active_checked_with_capacity(run_id, max_pending) {
            ActivePersistOutcome::Saved => {
                self.claims_pending_persist.remove(run_id);
                self.release_claim_on_park(run_id);
                ParkPersistOutcome::Released
            }
            ActivePersistOutcome::CapacityFull => ParkPersistOutcome::CapacityFull,
            ActivePersistOutcome::Failed => {
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
                ParkPersistOutcome::PersistFailed
            }
        }
    }

    /// Retry the durable persist for every run in `claims_pending_persist`. A
    /// retry that now succeeds completes the deferred park transition (releases
    /// the claim). A retry that still fails, or that now finds the pending pool
    /// full, leaves the run tracked - but the persist helper heartbeats the claim
    /// BEFORE attempting the write, unconditionally, so even an unsaved retry still
    /// renews the kept claim's lease. This is what keeps `reap_expired_claims`
    /// from reclaiming it: called every maintenance tick, a park that never
    /// manages to persist still gets its claim renewed once per tick for as long
    /// as it stays parked.
    fn retry_pending_park_persists(&mut self) {
        let pending: Vec<String> = self.claims_pending_persist.iter().cloned().collect();
        for run_id in pending {
            let Some(status) = self.active_runs.get(&run_id).map(|run| run.status) else {
                // The run left active_runs some other way (finished/evicted);
                // nothing left to retry or release.
                self.claims_pending_persist.remove(&run_id);
                continue;
            };
            let max_pending = self.pending_capacity_limit_for_run(&run_id);
            match self.persist_active_checked_with_capacity(&run_id, max_pending) {
                ActivePersistOutcome::Saved => {
                    self.claims_pending_persist.remove(&run_id);
                    // Only release the claim if the run is STILL parked. The entry
                    // guards in `resolve_gate`/`approve_step`/`resume_deterministic_run`
                    // (`is_park_persist_pending`) already refuse to resume a run while
                    // it is tracked here, so this should be unreachable in practice -
                    // but if a run somehow left the parked state without going through
                    // one of those guarded paths, its claim is now legitimately held
                    // by that transition and must NOT be released out from under it.
                    if !holds_exec_claim(status) {
                        self.release_claim_on_park(&run_id);
                    }
                }
                ActivePersistOutcome::CapacityFull | ActivePersistOutcome::Failed => {}
            }
        }
    }

    fn retry_capacity_blocked_gated_pends(&mut self) {
        let candidates: Vec<String> = self
            .active_runs
            .values()
            .filter(|run| run.status == SopRunStatus::Pending)
            .map(|run| run.run_id.clone())
            .collect();

        for run_id in candidates {
            let Some((sop, step)) = self.active_runs.get(&run_id).and_then(|run| {
                let sop = self.sops.iter().find(|sop| sop.name == run.sop_name)?;
                // Resolve the gated step by NUMBER, not vector index: step numbers
                // are not required to be contiguous/1-based, so an index lookup
                // strands a non-contiguous pending step (it never re-promotes and
                // leaks its exec claim).
                let step = sop
                    .steps
                    .iter()
                    .find(|step| step.number == run.current_step)?;
                pending_step_blocks_direct_advance(sop, step).then(|| (sop.clone(), step.clone()))
            }) else {
                continue;
            };

            if self.pending_pool_full_reason(&sop).is_some() {
                continue;
            }

            if step.kind == SopStepKind::Checkpoint {
                if let Err(e) = self.persist_deterministic_state(&run_id, &sop, true) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "error": e.to_string(),
                            })),
                        "SOP maintenance: checkpoint pending-cap retry could not persist state"
                    );
                    continue;
                }
                if let Some(run) = self.active_runs.get_mut(&run_id) {
                    run.status = SopRunStatus::PausedCheckpoint;
                    run.waiting_since = Some(now_iso8601());
                }
            } else if let Some(run) = self.active_runs.get_mut(&run_id) {
                run.status = SopRunStatus::WaitingApproval;
                run.waiting_since = Some(now_iso8601());
            }

            match self.persist_parked_snapshot_then_release_claim(&run_id) {
                ParkPersistOutcome::Released | ParkPersistOutcome::PersistFailed => {}
                ParkPersistOutcome::CapacityFull => {
                    let reason = self.pending_pool_capacity_raced_reason(&sop);
                    Self::log_pending_capacity_full(&run_id, &reason);
                    self.mark_step_pending(&run_id, &sop, step.number, reason);
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
        for run_id in &self.claims_retained_after_terminal_rollback {
            if let Some(run) = self.active_runs.get(run_id)
                && !holds_exec_claim(run.status)
            {
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

    /// Checked counterpart to `release_claim_on_park`: release a parked run's exec
    /// claim and REPORT a store failure instead of swallowing it. Used on the
    /// checkpoint-denial CONTINUATION path, where the reacquired claim still carries
    /// the durable terminal-rollback retention marker. If that release is swallowed
    /// and fails, the marker survives on a run that actually CONTINUED (did not
    /// terminal-rollback), and `restore_runs` would then renew that stale claim
    /// forever, leaking the slot. Returning the error lets the caller fail closed
    /// (roll back + surface it) rather than report success with a live marker.
    /// `Ok(())` when there is no such active run (nothing to release).
    fn release_claim_checked(&self, run_id: &str) -> Result<(), crate::sop::store::StoreError> {
        match self.active_runs.get(run_id) {
            Some(run) => self.store.release_claim(&Self::claim_handle_for_run(run)),
            None => Ok(()),
        }
    }

    /// Whether a durable terminal-rollback retention marker on a restored parked
    /// run is STALE. A legitimate marker guards a run whose TERMINAL write failed and
    /// left it restorable in its pre-terminal parked state — still awaiting the
    /// retried decision at its current checkpoint, which therefore has NO recorded
    /// result yet. If the current step ALREADY has a recorded `step_result`, the run
    /// reached this parked gate through a COMPLETED failure-route continuation (e.g. a
    /// denied checkpoint that `Retry`-re-parked at the same step), so the marker is
    /// stale and must be released rather than renewed forever.
    ///
    /// This is a HEURISTIC, not an exact classifier, and it errs on the SAFE side.
    /// It has two disclosed residuals, both bounded and benign:
    /// - It does NOT catch a denial that routed via `Goto` to a DIFFERENT, fresh
    ///   checkpoint (new current step, no result yet): that durable footprint is
    ///   indistinguishable from a legitimate terminal rollback at that fresh checkpoint,
    ///   so a stale marker there survives. The checked continuation release plus the
    ///   lease reaper cover that path in the non-crash case (see `deny_checkpoint`).
    /// - Symmetrically, it CAN flag a legitimate marker: a `Retry` checkpoint denied
    ///   enough times to re-park at the same step (leaving a `Failed` result there) and
    ///   then routed to a terminal `Fail` whose terminal write fails takes
    ///   `deny_checkpoint`'s retain-and-restore branch while carrying a result for its
    ///   current step; a restart before re-resolution would release that legitimate
    ///   marker. That direction is safe: the run is still restored into `active_runs`
    ///   (never lost) and only loses its HELD slot, degrading to standard parked
    ///   semantics — it re-acquires its exec slot on its next decision, capped
    ///   (subject to `max_concurrent`/`max_concurrent_total`) via
    ///   `reacquire_claim_on_resume` for an approval or checkpoint-approve resume, or
    ///   uncapped via `reacquire_claim_uncapped` for a subsequent denial. No double
    ///   execution, no permanent leak, no hard-cap violation.
    fn terminal_rollback_marker_is_stale(run: &SopRun) -> bool {
        run.step_results
            .iter()
            .any(|result| result.step_number == run.current_step)
    }

    /// A1: re-establish a RESUMING run's exec claim, subject to the SOP's per-SOP
    /// `max_concurrent` AND the global `max_concurrent_total`. A run parked at a HITL
    /// approval / deterministic checkpoint released its exec slot on park; resuming
    /// it must re-admit through the SAME store CAS (`try_claim_run`) a fresh start
    /// uses, so a burst of simultaneous approvals can never push executing runs past
    /// the configured caps. (That burst is the reviewed defect: many runs park,
    /// releasing their slots, then all resume at once - the uncapped restore path
    /// let them oversubscribe.) Three outcomes:
    /// - `Ok(())`                 a slot was available; the run holds its claim and may resume.
    /// - `Err(ResumeAtCapacity)`  the cap is saturated. TYPED backpressure, NOT a fault:
    ///   the caller leaves the run parked and re-resolvable (`resolve_gate` reports
    ///   `DeferredAtCapacity`; the checkpoint paths surface it to the operator), and a
    ///   later approval attempt or the timeout tick's retry resumes it once a slot frees.
    /// - `Err(_)`                 a store fault (fail-closed, as before): abort the resume,
    ///   never execute uncounted.
    ///
    /// A missing run is a no-op `Ok` (the caller already validated it exists). The
    /// checkpoint-DENIAL path uses `reacquire_claim_uncapped` instead - a denial may
    /// TERMINATE the run, and gating a terminating run on a free slot would refuse to
    /// end it under load and strand it.
    pub(crate) fn reacquire_claim_on_resume(&self, run_id: &str) -> Result<()> {
        let Some((rid, sop_name)) = self
            .active_runs
            .get(run_id)
            .map(|run| (run.run_id.clone(), run.sop_name.clone()))
        else {
            return Ok(());
        };
        // Resolve the per-SOP cap exactly as the normal admit path does. The resume
        // pre-flights (`can_clear_waiting_gate` / `can_advance_deterministic_step`)
        // already proved the SOP is still loaded before we reach here; if it somehow
        // is not, fail closed rather than resume uncounted.
        let per_sop_cap = self
            .get_sop(&sop_name)
            .map(|sop| sop.max_concurrent as usize);
        let Some(per_sop_cap) = per_sop_cap else {
            return Err(anyhow::Error::msg(format!(
                "failed to re-acquire exec claim on resume for run {rid}: SOP '{sop_name}' no longer loaded"
            )));
        };
        match self.store.try_claim_run(
            &rid,
            &sop_name,
            per_sop_cap,
            self.config.max_concurrent_total,
        ) {
            Ok(Some(_token)) => Ok(()),
            Ok(None) => Err(anyhow::Error::new(ResumeAtCapacity {
                run_id: rid,
                sop_name,
            })),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "run_id": rid.as_str(),
                            "error": e.to_string(),
                        })),
                    "SOP engine: resume aborted, could not re-acquire the run admission claim (fail-closed)"
                );
                Err(anyhow::Error::msg(format!(
                    "failed to re-acquire exec claim on resume for run {rid}: {e}"
                )))
            }
        }
    }

    /// UNCAPPED exec-claim re-establishment, for the checkpoint-DENIAL path only
    /// (`deny_checkpoint`). A denial may TERMINATE the run - it reacquires the claim
    /// to write terminal state and the terminal-rollback retention marker atomically,
    /// so this is rollback/atomicity machinery, not new admission, and must never be
    /// blocked by the concurrency cap (refusing to terminate a run under load would
    /// strand it, since it already released its slot at park). This is the ORIGINAL
    /// uncapped restore behavior; the capped `reacquire_claim_on_resume` above governs
    /// the three resume-to-continue paths (approval approve, checkpoint approve,
    /// deterministic resume). Fail-CLOSED on a store error, as before.
    pub(crate) fn reacquire_claim_uncapped(&self, run_id: &str) -> Result<()> {
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

    /// Persist a run that has reached a terminal state and release its claim atomically.
    fn persist_terminal(&self, run: &SopRun) -> Result<()> {
        let mut pr = PersistedRun::new(run.clone(), now_iso8601(), run.trigger_event.source);
        // The terminal write is the run's final revision; advance past the last
        // active snapshot so the store's revision guard accepts it.
        pr.revision = self.next_run_revision(&run.run_id);
        self.store.finish_run(&run.run_id, &pr).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(
                        ::serde_json::json!({"run_id": run.run_id, "error": e.to_string()})
                    ),
                "SOP engine: terminal persistence failed; run and admission claim remain active"
            );
            anyhow::Error::new(TerminalPersistenceRetained {
                run_id: run.run_id.clone(),
                source: e,
            })
        })?;
        self.notify_run(run, false);
        Ok(())
    }

    /// Terminal counterpart to `persist_active_with_gate_event`: persist the
    /// terminal run, release its claim, and append the gate-resolution ledger row
    /// in one store transaction.
    fn persist_terminal_with_gate_event(&self, run: &SopRun, event: &SopEventRecord) -> Result<()> {
        let mut pr = PersistedRun::new(run.clone(), now_iso8601(), run.trigger_event.source);
        pr.revision = self.next_run_revision(&run.run_id);
        self.store
            .finish_run_with_event(&run.run_id, &pr, event)
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(
                            ::serde_json::json!({"run_id": run.run_id, "error": e.to_string()})
                        ),
                    "SOP engine: terminal gate resolution persistence failed; run and ledger remain uncommitted"
                );
                anyhow::Error::new(e)
            })?;
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
    pub(crate) fn exec_counts(&self, sop_name: &str) -> (usize, usize) {
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

    fn rollback_failed_start(
        &mut self,
        run_id: &str,
        claim: &ClaimToken,
        err: anyhow::Error,
    ) -> anyhow::Error {
        if err.is::<TerminalPersistenceRetained>() {
            return err;
        }
        self.active_runs.remove(run_id);
        self.release_claim_best_effort(claim);
        err
    }

    /// Undo a SUCCESSFUL `activate_reserved_run` that must be reversed because a LATER
    /// sibling in the same all-or-nothing AMQP multi-match batch failed to activate.
    /// Activation runs no irreversible side effect (deterministic execution and the LLM
    /// agent loop both run LATER, in `record_started_run` / the driver), so the run is
    /// safe to reverse. Two cases:
    /// - A still-EXECUTING sibling (`holds_exec_claim` true) never durably persisted during
    ///   activation: drop it in-memory and release its exec claim.
    /// - A sibling that PARKED at a step-1 approval/checkpoint gate DID durably persist its
    ///   parked snapshot (and already released its claim). Dropping it only in-memory would
    ///   ORPHAN that durable row: after a restart, `restore_runs` would reconstruct it,
    ///   duplicating a run whose whole delivery was deferred + requeued. Durably supersede it
    ///   with a terminal `Cancelled` (a higher revision the store's guard accepts) so restore
    ///   skips it. Best-effort: a store failure here only leaves the bounded orphan back
    ///   (logged), never a double execution — the sibling never ran.
    pub(crate) fn rollback_activated_run(&mut self, run_id: &str) {
        let Some(mut run) = self.active_runs.remove(run_id) else {
            return;
        };
        if holds_exec_claim(run.status) {
            self.release_claim_best_effort(&Self::claim_handle_for_run(&run));
            return;
        }
        // Parked sibling: its durable snapshot must not survive the rollback.
        run.status = SopRunStatus::Cancelled;
        run.completed_at = Some(now_iso8601());
        if let Err(e) = self.persist_terminal(&run) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "run_id": run.run_id.as_str(),
                        "error": e.to_string(),
                    })),
                "SOP dispatch: could not durably cancel a rolled-back parked AMQP sibling; a stale parked row may be reconstructed on restart"
            );
        }
    }

    pub fn start_run(&mut self, sop_name: &str, event: SopEvent) -> Result<SopRunAction> {
        // A start is a two-phase operation: reserve the exec slot through the
        // authoritative store CAS (no side effect yet), then activate the reserved
        // slot into a live run and dispatch its first step. The phases are split so the
        // AMQP multi-match path can reserve the WHOLE matched batch before activating
        // any of it (see `dispatch`). A single start runs both phases back-to-back.
        let reservation = self.reserve_run_slot(sop_name)?;
        self.activate_reserved_run(reservation, event)
    }

    /// Phase 1 of a start: reserve `sop_name`'s exec slot through the authoritative
    /// store CAS WITHOUT creating an active run or dispatching any step — so no SOP
    /// side effect occurs yet. The returned `StartReservation` holds a live claim; the
    /// caller MUST either `activate_reserved_run` it or `release_reservation` it, or
    /// the slot leaks. This is the primitive behind the AMQP multi-match all-or-defer-
    /// all reservation: every matched SOP's capacity is held atomically before ANY of
    /// them produces a side effect, so a sibling engine grabbing a slot mid-batch can
    /// never leave a partial start (it makes one reservation fail → release-all +
    /// defer-all), only a safe requeue.
    pub(crate) fn reserve_run_slot(&mut self, sop_name: &str) -> Result<StartReservation> {
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

        let deterministic = sop.execution_mode == SopExecutionMode::Deterministic;
        self.run_counter += 1;
        let dur = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let epoch_ns = dur.as_nanos();
        let prefix = if deterministic { "det" } else { "run" };
        let run_id = format!("{prefix}-{epoch_ns}-{:04}", self.run_counter);
        let claim = self.claim_admission(&run_id, &sop)?;
        Ok(StartReservation {
            run_id,
            claim,
            sop,
            deterministic,
        })
    }

    /// Release a reservation that will NOT be activated (a batch that could not fully
    /// reserve), freeing its exec slot for admission. Best-effort + logged, exactly
    /// like a park release: a swallowed failure only lets the reaper collect the claim
    /// later — no run was ever created, so there is no side effect to unwind.
    pub(crate) fn release_reservation(&self, reservation: StartReservation) {
        self.release_claim_best_effort(&reservation.claim);
    }

    /// Phase 2 of a start: convert a held reservation into a live run — build the run
    /// record, insert it, and dispatch its first step, rolling the reservation back
    /// (release the claim, drop the run) if that dispatch fails.
    pub(crate) fn activate_reserved_run(
        &mut self,
        reservation: StartReservation,
        event: SopEvent,
    ) -> Result<SopRunAction> {
        let StartReservation {
            run_id,
            claim,
            sop,
            deterministic,
        } = reservation;

        let run = SopRun {
            run_id: run_id.clone(),
            sop_name: sop.name.clone(),
            trigger_event: event,
            frame_marker_id: new_marker_id(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: u32::try_from(sop.steps.len()).unwrap_or(u32::MAX),
            started_at: now_iso8601(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
        };
        self.active_runs.insert(run_id.clone(), run);

        if deterministic {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Deterministic SOP run {} started for '{}'",
                    run_id, sop.name
                )
            );
            match self.dispatch_deterministic_step(&run_id, &sop, 1, serde_json::Value::Null) {
                Ok(action) => Ok(action),
                Err(e) => Err(self.rollback_failed_start(&run_id, &claim, e)),
            }
        } else {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!("SOP run {} started for '{}'", run_id, sop.name)
            );
            match self.dispatch_llm_step(&run_id, &sop, 1, None) {
                Ok(action) => Ok(action),
                Err(e) => Err(self.rollback_failed_start(&run_id, &claim, e)),
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

        if self
            .active_runs
            .get(run_id)
            .is_some_and(|run| run.status == SopRunStatus::Pending)
            && pending_step_blocks_direct_advance(&sop, &current_step)
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "run_id": run_id,
                        "step": current_step.number,
                        "step_kind": current_step.kind.to_string(),
                    })),
                "SOP engine: advance_step rejected - pending run is blocked at a human gate"
            );
            bail!(
                "Run {run_id} is pending at gated step {}; wait for pending approval/checkpoint \
                 capacity and resolve the gate before advancing with sop_advance",
                current_step.number
            );
        }

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
    ) -> Result<Option<SopRunAction>> {
        self.schema_input_failure_reason(step, input)
            .map(|reason| self.fail_step_schema_validation(run_id, step.number, "input", reason))
            .transpose()
    }

    fn schema_input_failure_reason(&self, step: &SopStep, input: &Value) -> Option<String> {
        self.validate_step_input(step, input).err()
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
    ) -> Result<SopRunAction> {
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

    fn gate_schema_failure_transition(
        &self,
        run_id: &str,
        step_number: u32,
        phase: &'static str,
        reason: String,
    ) -> Result<GateClearTransition> {
        self.active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        let reason = format!("Step {step_number} {phase} schema validation failed: {reason}");
        Ok(GateClearTransition::Terminal {
            status: SopRunStatus::Failed,
            reason: Some(reason.clone()),
            follow_up: Some(GateResolutionFollowUp::StepSchemaReject {
                step: step_number,
                phase,
                reason,
            }),
        })
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
                    self.finish_deterministic_run(run_id)
                } else {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"run_id": run_id})),
                        "SOP run completed successfully"
                    );
                    self.finish_run(run_id, SopRunStatus::Completed, None)
                }
            }
            NextStep::Fail(reason) => self.finish_run(run_id, SopRunStatus::Failed, Some(reason)),
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
        )?))
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
        if let Some(action) = self.schema_input_failure_action(run_id, &step, &input)? {
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

        // A1: free the exec slot while the run waits on a human - but only AFTER
        // the parked snapshot is durably persisted (else keep the claim, fail
        // closed).
        if parked_for_approval {
            if let Some(reason) = self.pending_pool_full_reason(sop) {
                Self::log_pending_capacity_full(run_id, &reason);
                return Ok(self.mark_step_pending(run_id, sop, step.number, reason));
            }
            if let Some(run) = self.active_runs.get_mut(run_id) {
                run.status = SopRunStatus::WaitingApproval;
                run.waiting_since = Some(now_iso8601());
            }
            match self.persist_parked_snapshot_then_release_claim(run_id) {
                ParkPersistOutcome::Released => {}
                ParkPersistOutcome::CapacityFull => {
                    let reason = self.pending_pool_capacity_raced_reason(sop);
                    Self::log_pending_capacity_full(run_id, &reason);
                    return Ok(self.mark_step_pending(run_id, sop, step.number, reason));
                }
                ParkPersistOutcome::PersistFailed => {
                    let reason =
                        format!("SOP '{}' park snapshot not yet durably persisted", sop.name);
                    return Ok(SopRunAction::Pending {
                        run_id: run_id.to_string(),
                        sop_name: sop.name.clone(),
                        step: step.number,
                        reason,
                    });
                }
            }
        } else {
            self.persist_active(run_id);
        }
        Ok(action)
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
        self.mark_step_pending_with_persist(run_id, sop, step_number, reason, true)
    }

    fn mark_step_pending_with_persist(
        &mut self,
        run_id: &str,
        sop: &Sop,
        step_number: u32,
        reason: String,
        persist: bool,
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
        if persist {
            self.persist_active(run_id);
        }
        SopRunAction::Pending {
            run_id: run_id.to_string(),
            sop_name: sop.name.clone(),
            step: step_number,
            reason,
        }
    }

    fn gate_step_pending_transition(
        &mut self,
        run_id: &str,
        sop: &Sop,
        step_number: u32,
        reason: String,
    ) -> Result<GateClearTransition> {
        let now = now_iso8601();
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
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
                completed_at: Some(now),
                tool_calls: Vec::new(),
            });
        }

        Ok(GateClearTransition::Active {
            action: Box::new(SopRunAction::Pending {
                run_id: run_id.to_string(),
                sop_name: sop.name.clone(),
                step: step_number,
                reason: reason.clone(),
            }),
            follow_up: Some(GateResolutionFollowUp::StepSkipped {
                sop_name: sop.name.clone(),
                step: step_number,
                reason,
            }),
        })
    }

    fn record_gate_resolution_follow_up(&self, run_id: &str, follow_up: GateResolutionFollowUp) {
        match follow_up {
            GateResolutionFollowUp::StepSchemaReject {
                step,
                phase,
                reason,
            } => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "run_id": run_id,
                            "step": step,
                            "phase": phase,
                            "reason": reason.as_str(),
                        })),
                    "SOP step schema validation failed"
                );
                self.record_transition_event(
                    run_id,
                    "step_schema_reject",
                    Some(reason),
                    ::serde_json::json!({
                        "step": step,
                        "phase": phase,
                    }),
                );
            }
            GateResolutionFollowUp::StepSkipped {
                sop_name,
                step,
                reason,
            } => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "run_id": run_id,
                            "sop_name": sop_name,
                            "step": step,
                            "reason": reason.as_str(),
                        })),
                    "SOP run pending on step dependencies"
                );
                self.record_transition_event(
                    run_id,
                    "step_skipped",
                    Some(reason),
                    ::serde_json::json!({
                        "step": step,
                        "status": "pending",
                    }),
                );
            }
        }
    }

    fn finish_deterministic_run(&mut self, run_id: &str) -> Result<SopRunAction> {
        let saved = self
            .active_runs
            .get(run_id)
            .map(|run| run.llm_calls_saved)
            .unwrap_or(0);
        let action = self.finish_run(run_id, SopRunStatus::Completed, None)?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("Deterministic SOP run {run_id} completed ({saved} LLM calls saved)")
        );
        self.deterministic_savings.total_llm_calls_saved += saved;
        self.deterministic_savings.total_runs += 1;
        Ok(action)
    }

    /// Cancel an active run.
    pub fn cancel_run(&mut self, run_id: &str) -> Result<()> {
        if !self.active_runs.contains_key(run_id) {
            bail!("Active run not found: {run_id}");
        }
        self.finish_run(run_id, SopRunStatus::Cancelled, None)?;
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
        self.reacquire_claim_on_resume(run_id)?;
        // A deterministic run paused at a checkpoint resumes through the
        // deterministic piping path: the checkpoint step is recorded as
        // completed and its output (or the previous step's) is piped forward.
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        let piped = run
            .step_results
            .last()
            .map(step_result_value)
            .unwrap_or(serde_json::Value::Null);
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

        if self.is_park_persist_pending(run_id) {
            bail!(
                "Run {run_id} cannot resolve: its parked checkpoint snapshot is not yet durably persisted (retrying)"
            );
        }

        let (_, sop) = self.resolve_active_run_sop(run_id)?;
        let current_step_number = self
            .active_runs
            .get(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?
            .current_step;
        let current_step = self.resolve_sop_step(&sop, current_step_number)?;

        // Resolve a failure-route target before mutating the parked run. A stale
        // `Goto` must leave the checkpoint untouched and re-resolvable.
        if let super::step_contract::StepFailure::Goto { step } = &current_step.on_failure {
            self.resolve_sop_step(&sop, *step)?;
        }

        let prior_run = self
            .active_runs
            .get(run_id)
            .cloned()
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        // Classify the denial's routing outcome BEFORE any mutation, using the
        // AUTHORITATIVE failure router (not a second copy of its logic). A denial
        // records the checkpoint step `Failed`; the router computes `retries_consumed`
        // as (Failed count - 1) after that record, so before it the current Failed
        // count for this step is exactly that value.
        let retries_consumed = self
            .active_runs
            .get(run_id)
            .map(|run| {
                run.step_results
                    .iter()
                    .filter(|r| {
                        r.step_number == current_step.number && r.status == SopStepStatus::Failed
                    })
                    .count() as u32
            })
            .unwrap_or(0);
        let terminates = matches!(
            route::failure::route_failure(
                &current_step.on_failure,
                retries_consumed,
                self.config.max_step_retries,
            ),
            NextStep::Fail(_)
        );
        if terminates {
            // TERMINAL denial (default `Fail`, or a `Retry` whose budget is spent):
            // it must reacquire to complete atomically even under saturation - gating
            // a run that is ENDING on a free slot would strand it. This is the
            // terminal-rollback atomicity path; it stays UNCAPPED by design.
            self.reacquire_claim_uncapped(run_id)?;
        } else {
            // CONTINUING denial (`Goto`, or a `Retry` with budget remaining): it
            // resumes execution, so it must pass the SAME capped store CAS every other
            // resume-to-continue path uses, honoring the per-SOP and global limits. At
            // capacity this returns `ResumeAtCapacity`; the `?` early-returns with the
            // checkpoint still parked and re-resolvable (no mutation, no retention
            // marker yet) - typed backpressure, never an over-cap execution.
            self.reacquire_claim_on_resume(run_id)?;
        }
        if let Err(marker_err) = self
            .store
            .mark_claim_retained_after_terminal_rollback(run_id)
        {
            self.active_runs.insert(run_id.to_string(), prior_run);
            self.release_claim_on_park(run_id);
            return Err(anyhow::Error::msg(format!(
                "failed to persist terminal-rollback claim retention marker for run {run_id}: {marker_err}"
            )));
        }
        self.claims_retained_after_terminal_rollback
            .insert(run_id.to_string());

        let detail = reason.unwrap_or_else(|| "checkpoint denied by operator".to_string());
        let now = now_iso8601();

        if let Some(run) = self.active_runs.get_mut(run_id) {
            run.status = SopRunStatus::Running;
            run.waiting_since = None;
        }
        match self.record_deterministic_step_result(
            run_id,
            &sop,
            &current_step,
            SopStepStatus::Failed,
            detail.clone(),
            serde_json::Value::String(detail.clone()),
            now.clone(),
            Some(now),
        ) {
            Ok(action) => {
                if !self.persist_active_checked(run_id) {
                    self.active_runs.insert(run_id.to_string(), prior_run);
                    self.claims_pending_persist.remove(run_id);
                    self.claims_retained_after_terminal_rollback.remove(run_id);
                    self.release_claim_on_park(run_id);
                    return Err(anyhow::Error::msg(format!(
                        "failed to persist checkpoint denial transition for run {run_id}"
                    )));
                }
                if self.active_runs.get(run_id).is_some_and(|run| {
                    matches!(
                        run.status,
                        SopRunStatus::WaitingApproval | SopRunStatus::PausedCheckpoint
                    )
                }) {
                    // The denial ROUTED to another gate and the new parked snapshot
                    // is durably persisted, so this run continued — it did NOT terminal-
                    // rollback. The reacquired claim still carries the durable terminal-
                    // rollback retention marker, which is now stale. Clear it with a
                    // CHECKED release: a swallowed failure would leave a live durable
                    // marker on a continued run, which `restore_runs` would then renew
                    // forever (the slot leak this PR exists to prevent). If the release
                    // fails we must NOT report success with a live marker — roll back to
                    // the pre-decision park, drop the in-memory retention/pending
                    // tracking (so the stale claim is not heartbeated and the lease
                    // reaper frees it), and surface the error so the caller retries.
                    if let Err(e) = self.release_claim_checked(run_id) {
                        self.active_runs.insert(run_id.to_string(), prior_run);
                        self.claims_pending_persist.remove(run_id);
                        self.claims_retained_after_terminal_rollback.remove(run_id);
                        return Err(anyhow::Error::msg(format!(
                            "failed to release exec claim after routing checkpoint denial for run {run_id}: {e}"
                        )));
                    }
                    self.claims_pending_persist.remove(run_id);
                }
                self.claims_retained_after_terminal_rollback.remove(run_id);
                self.record_transition_event(
                    run_id,
                    "checkpoint_denied",
                    Some(detail),
                    ::serde_json::json!({
                        "step": current_step.number,
                        "kind": current_step.kind.to_string(),
                    }),
                );
                Ok(action)
            }
            Err(e) => {
                self.active_runs.insert(run_id.to_string(), prior_run);
                // The terminal write was rejected, so the durable store may still
                // restore this parked run. Keep the claim acquired for this decision
                // attempt to prevent another trigger from taking its execution slot.
                Err(e)
            }
        }
    }

    /// Prepare a `WaitingApproval` gate clear: mutate the in-memory run to the
    /// target state and describe how the wrapper must commit it with the gate
    /// ledger row. The wrapper owns persistence and post-commit secondary events.
    ///
    /// All-or-nothing: the SOP definition and current step are resolved (and
    /// bounds-checked) BEFORE any in-memory mutation, so a definition removed or
    /// shrunk mid-run returns `Err` with the gate left untouched (still
    /// `WaitingApproval`, re-resolvable) rather than half-transitioned or panicking
    /// on an out-of-range step index (which would poison the engine mutex). The
    /// pure prefix of these lookups is exposed as `can_clear_waiting_gate` so
    /// `resolve_gate` can fail closed before it touches the claim or the ledger.
    fn clear_waiting_gate(&mut self, run_id: &str) -> Result<GateClearTransition> {
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
            return self.gate_step_pending_transition(
                run_id,
                &sop,
                step.number,
                format!("step {} dependencies not satisfied", step.number),
            );
        }

        let input = {
            let run = self
                .active_runs
                .get(run_id)
                .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
            step_input_value(run, step.number)
        };
        if let Some(reason) = self.schema_input_failure_reason(&step, &input) {
            return self.gate_schema_failure_transition(run_id, step.number, "input", reason);
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

        Ok(GateClearTransition::Active {
            action: Box::new(SopRunAction::ExecuteStep {
                run_id: run_id.to_string(),
                step,
                context,
            }),
            follow_up: None,
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

        let sop = self.get_sop(sop_name).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"sop_name": sop_name})),
                "SOP engine: sop not found"
            );
            anyhow::Error::msg(format!("SOP not found: {sop_name}"))
        })?;

        // Reject a non-deterministic SOP BEFORE reserving a slot, so a wrong-mode direct
        // call cannot claim (and then have to roll back) an execution slot.
        if sop.execution_mode != SopExecutionMode::Deterministic {
            bail!(
                "SOP '{}' is not in deterministic mode (mode: {})",
                sop_name,
                sop.execution_mode
            );
        }

        // Reserve + activate through the shared two-phase start path (identical run_id
        // prefix, logging, and dispatch to the pre-refactor inline body).
        let reservation = self.reserve_run_slot(sop_name)?;
        self.activate_reserved_run(reservation, event)
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
                    return self.fail_headless_driverless_step(run_id, &sop_name, step);
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

    fn execute_capability_step(
        &mut self,
        sop: &Sop,
        run_id: &str,
        step: &SopStep,
        input: serde_json::Value,
    ) -> Result<SopRunAction> {
        let started_at = now_iso8601();
        let ctx = super::capability::CapabilityContext {
            run_id: run_id.to_string(),
            sop_name: sop.name.clone(),
            step_number: step.number,
            sop_location: sop.location.clone(),
        };
        let result = self.capabilities.execute_step(ctx, step, input);
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
    ) -> Result<SopRunAction> {
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

        if let Some(action) = self.schema_input_failure_action(run_id, step, &input)? {
            return Ok(action);
        }

        match step.kind {
            SopStepKind::Checkpoint => {
                if let Some(reason) = self.pending_pool_full_reason(sop) {
                    Self::log_pending_capacity_full(run_id, &reason);
                    return Ok(self.mark_step_pending(run_id, sop, step.number, reason));
                }

                // Persist the checkpoint state before flipping the run status. If
                // the state-file write fails, the run remains Running with its
                // execution claim still heartbeat-eligible.
                let state_file = self.persist_deterministic_state(run_id, sop, true)?;

                // Pause at checkpoint - persist state and wait for approval
                if let Some(run) = self.active_runs.get_mut(run_id) {
                    run.status = SopRunStatus::PausedCheckpoint;
                    run.waiting_since = Some(now_iso8601());
                }

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
                // row for restore_runs() to rehydrate. A1: free the exec slot while
                // the run waits at the checkpoint - but only AFTER the parked
                // snapshot is durably persisted (else keep the claim).
                match self.persist_parked_snapshot_then_release_claim(run_id) {
                    ParkPersistOutcome::Released => {}
                    ParkPersistOutcome::CapacityFull => {
                        let reason = self.pending_pool_capacity_raced_reason(sop);
                        Self::log_pending_capacity_full(run_id, &reason);
                        return Ok(self.mark_step_pending(run_id, sop, step.number, reason));
                    }
                    ParkPersistOutcome::PersistFailed => {
                        let reason =
                            format!("SOP '{}' park snapshot not yet durably persisted", sop.name);
                        return Ok(SopRunAction::Pending {
                            run_id: run_id.to_string(),
                            sop_name: sop.name.clone(),
                            step: step.number,
                            reason,
                        });
                    }
                }

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
    fn persist_deterministic_state(
        &self,
        run_id: &str,
        sop: &Sop,
        paused_at_checkpoint: bool,
    ) -> Result<PathBuf> {
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
            paused_at_checkpoint,
        };

        // Write to SOP location directory, or system temp dir
        let temp_dir = std::env::temp_dir();
        let dir = sop.location.as_deref().unwrap_or(temp_dir.as_path());
        let state_file = dir.join(format!("{run_id}.state.json"));
        let json = serde_json::to_string_pretty(&state)?;
        std::fs::write(&state_file, json)?;

        Ok(state_file)
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
            .filter(|r| !self.is_park_persist_pending(&r.run_id))
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
        self.retry_capacity_blocked_gated_pends();
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
    pub(crate) fn restamp_waiting_with_gate_event(
        &mut self,
        run_id: &str,
        event: &SopEventRecord,
    ) -> Result<()> {
        let previous = match self.active_runs.get_mut(run_id) {
            Some(run) => {
                let previous = run.waiting_since.clone();
                run.waiting_since = Some(now_iso8601());
                previous
            }
            None => return Ok(()),
        };
        // Persist the re-stamped clock with the escalation event as one durable
        // outcome; otherwise history could say the gate escalated while the
        // timeout clock still points at the old overdue instant.
        if let Err(e) = self.persist_active_with_gate_event(run_id, event) {
            if let Some(run) = self.active_runs.get_mut(run_id) {
                run.waiting_since = previous;
            }
            return Err(e);
        }
        Ok(())
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
    ) -> Result<SopRunAction> {
        let mut run = self
            .active_runs
            .get(run_id)
            .cloned()
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        run.status = status;
        run.completed_at = Some(now_iso8601());
        let sop_name = run.sop_name.clone();
        let run_id_owned = run.run_id.clone();
        self.persist_terminal(&run)?;
        self.claims_pending_persist.remove(run_id);
        self.claims_retained_after_terminal_rollback.remove(run_id);
        self.active_runs.remove(run_id);
        self.metrics.record_run_complete(&run);
        self.finished_runs.push(run);

        // Evict oldest finished runs when over capacity
        let max = self.config.max_finished_runs;
        if max > 0 && self.finished_runs.len() > max {
            let excess = self.finished_runs.len() - max;
            self.finished_runs.drain(..excess);
        }

        Ok(match status {
            SopRunStatus::Failed => SopRunAction::Failed {
                run_id: run_id_owned,
                sop_name,
                reason: reason.unwrap_or_default(),
            },
            _ => SopRunAction::Completed {
                run_id: run_id_owned,
                sop_name,
            },
        })
    }

    pub(crate) fn finish_run_with_gate_event(
        &mut self,
        run_id: &str,
        status: SopRunStatus,
        reason: Option<String>,
        event: &SopEventRecord,
    ) -> Result<SopRunAction> {
        let mut run = self
            .active_runs
            .get(run_id)
            .cloned()
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        run.status = status;
        run.completed_at = Some(now_iso8601());
        let sop_name = run.sop_name.clone();
        let run_id_owned = run.run_id.clone();
        self.persist_terminal_with_gate_event(&run, event)?;
        self.claims_pending_persist.remove(run_id);
        self.claims_retained_after_terminal_rollback.remove(run_id);
        self.active_runs.remove(run_id);
        self.metrics.record_run_complete(&run);
        self.finished_runs.push(run);

        let max = self.config.max_finished_runs;
        if max > 0 && self.finished_runs.len() > max {
            let excess = self.finished_runs.len() - max;
            self.finished_runs.drain(..excess);
        }

        Ok(match status {
            SopRunStatus::Failed => SopRunAction::Failed {
                run_id: run_id_owned,
                sop_name,
                reason: reason.unwrap_or_default(),
            },
            _ => SopRunAction::Completed {
                run_id: run_id_owned,
                sop_name,
            },
        })
    }

    pub(crate) fn clear_waiting_gate_with_event(
        &mut self,
        run_id: &str,
        event: &SopEventRecord,
    ) -> Result<SopRunAction> {
        let prior_run = self
            .active_runs
            .get(run_id)
            .cloned()
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        let action = match self.clear_waiting_gate(run_id) {
            Ok(transition) => match transition {
                GateClearTransition::Active { action, follow_up } => {
                    if let Err(e) = self.persist_active_with_gate_event(run_id, event) {
                        self.active_runs.insert(run_id.to_string(), prior_run);
                        self.release_claim_on_park(run_id);
                        return Err(e);
                    }
                    if let Some(follow_up) = follow_up {
                        self.record_gate_resolution_follow_up(run_id, follow_up);
                    }
                    *action
                }
                GateClearTransition::Terminal {
                    status,
                    reason,
                    follow_up,
                } => {
                    let action =
                        match self.finish_run_with_gate_event(run_id, status, reason, event) {
                            Ok(action) => action,
                            Err(e) => {
                                self.active_runs.insert(run_id.to_string(), prior_run);
                                self.release_claim_on_park(run_id);
                                return Err(e);
                            }
                        };
                    if let Some(follow_up) = follow_up {
                        self.record_gate_resolution_follow_up(run_id, follow_up);
                    }
                    action
                }
            },
            Err(e) => {
                self.active_runs.insert(run_id.to_string(), prior_run);
                self.release_claim_on_park(run_id);
                return Err(e);
            }
        };
        Ok(action)
    }

    // ── EPIC C: out-of-band approval plane ──────────────────────────

    /// Read-only config access for the approval resolver.
    pub fn config(&self) -> &SopConfig {
        &self.config
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

    /// Ordered event/ledger history for a run (from the durable store).
    pub fn run_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
        self.store.list_events(run_id)
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

fn step_requires_approval_gate(sop: &Sop, step: &SopStep) -> bool {
    if step.requires_confirmation {
        return true;
    }

    let effective_mode = step.mode.unwrap_or(sop.execution_mode);
    execution_mode_needs_approval(sop.execution_mode, sop, step)
        || execution_mode_needs_approval(effective_mode, sop, step)
}

fn pending_step_blocks_direct_advance(sop: &Sop, step: &SopStep) -> bool {
    step.kind == SopStepKind::Checkpoint || step_requires_approval_gate(sop, step)
}

/// Determine the action for a step based on the effective execution mode.
fn resolve_step_action(sop: &Sop, step: &SopStep, run_id: String, context: String) -> SopRunAction {
    let mut step = step.clone();
    step.agent = step
        .effective_agent(sop.agent.as_deref())
        .map(str::to_string);
    let step = &step;

    if step_requires_approval_gate(sop, step) {
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

fn step_input_value(run: &SopRun, step_number: u32) -> Value {
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
    fn start_run_terminal_persist_failure_retains_run_and_claim() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(true),
        });
        let mut sop = test_sop(
            "schema-start-finish-fail",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        );
        sop.steps[0].schema = Some(StepSchema {
            input: Some(required_object_schema("ok")),
            output: None,
        });
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        let err = engine
            .start_run(
                "schema-start-finish-fail",
                SopEvent {
                    source: SopTriggerSource::Manual,
                    topic: None,
                    payload: Some("{}".into()),
                    timestamp: now_iso8601(),
                },
            )
            .expect_err("terminal persistence failure must reject start");

        assert!(err.is::<TerminalPersistenceRetained>());
        assert!(err.to_string().contains("injected finish failure"));
        let run_id = first_active_run_id(&engine);
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Running,
            "failed terminal persistence must leave the start-path run active"
        );
        assert_eq!(
            store.claim_counts("schema-start-finish-fail").unwrap(),
            (1, 1),
            "failed terminal persistence must keep the admission claim"
        );
        assert!(
            engine.finished_runs(None).is_empty(),
            "the run must not move to terminal cache until terminal persistence succeeds"
        );
    }

    #[test]
    fn start_deterministic_terminal_persist_failure_retains_run_and_claim() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(true),
        });
        let mut sop = deterministic_sop_all_execute("det-schema-start-finish-fail");
        sop.steps[0].schema = Some(StepSchema {
            input: Some(required_object_schema("ok")),
            output: None,
        });
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        let err = engine
            .start_deterministic_run("det-schema-start-finish-fail", manual_event())
            .expect_err("terminal persistence failure must reject deterministic start");

        assert!(err.is::<TerminalPersistenceRetained>());
        assert!(err.to_string().contains("injected finish failure"));
        let run_id = first_active_run_id(&engine);
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Running,
            "failed terminal persistence must leave the deterministic run active"
        );
        assert_eq!(
            store.claim_counts("det-schema-start-finish-fail").unwrap(),
            (1, 1),
            "failed terminal persistence must keep the deterministic admission claim"
        );
        assert!(
            engine.finished_runs(None).is_empty(),
            "the deterministic run must not move to terminal cache until persistence succeeds"
        );
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
    fn pending_pool_cap_is_enforced_when_active_runs_reach_later_approval() {
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.max_concurrent = 2;
        sop.max_pending_approvals = 1;
        sop.steps[1].requires_confirmation = true;
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        let first = engine.start_run("s1", manual_event()).unwrap();
        let first_id = extract_run_id(&first).to_string();
        let second = engine.start_run("s1", manual_event()).unwrap();
        let second_id = extract_run_id(&second).to_string();
        assert_eq!(store.claim_counts("s1").unwrap(), (2, 2));

        let first_gate = engine
            .advance_step(
                &first_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "first".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(matches!(first_gate, SopRunAction::WaitApproval { .. }));
        assert_eq!(
            engine.get_run(&first_id).unwrap().status,
            SopRunStatus::WaitingApproval
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the first parked run released its exec claim"
        );
        assert_eq!(engine.pending_count_for_sop("s1"), 1);

        let second_blocked = engine
            .advance_step(
                &second_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "second".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            matches!(
                second_blocked,
                SopRunAction::Pending { step: 2, ref reason, .. }
                    if reason.contains("pending-approval pool full")
            ),
            "second run must not park past max_pending_approvals"
        );
        assert_eq!(
            engine.get_run(&second_id).unwrap().status,
            SopRunStatus::Pending
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the pending second run keeps its exec claim instead of parking claimless"
        );
        assert_eq!(
            engine.pending_count_for_sop("s1"),
            1,
            "only the first run counts against the pending approval pool"
        );
        let skipped = engine
            .advance_step(
                &second_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "unauthorized".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .expect_err("pending approval-cap backpressure must not be advanceable");
        assert!(
            skipped.to_string().contains("pending at gated step"),
            "unexpected advance error: {skipped}"
        );
        assert_eq!(
            engine.get_run(&second_id).unwrap().status,
            SopRunStatus::Pending,
            "the capped approval gate remains pending and cannot be bypassed"
        );
        let first_resumed = engine
            .resolve_gate(
                &first_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert!(matches!(first_resumed, ResolveOutcome::Resumed(_)));

        engine.run_maintenance_tick();
        assert_eq!(
            engine.get_run(&second_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "maintenance retries the blocked approval gate once pending capacity frees"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the recovered second gate releases its kept claim while waiting"
        );

        let second_resumed = engine
            .resolve_gate(
                &second_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert!(matches!(second_resumed, ResolveOutcome::Resumed(_)));
    }

    #[test]
    fn pending_checkpoint_cap_cannot_be_advanced_without_gate() {
        let mut sop = deterministic_sop("det-cp-cap");
        sop.max_concurrent = 2;
        sop.max_pending_approvals = 1;
        let mut engine = engine_with_sops(vec![sop]);

        let first = engine
            .start_deterministic_run("det-cp-cap", manual_event())
            .unwrap();
        let first_id = extract_run_id(&first).to_string();
        let second = engine
            .start_deterministic_run("det-cp-cap", manual_event())
            .unwrap();
        let second_id = extract_run_id(&second).to_string();

        let first_checkpoint = engine
            .advance_deterministic_step(&first_id, serde_json::json!("first"), None)
            .unwrap();
        assert!(matches!(
            first_checkpoint,
            SopRunAction::CheckpointWait { .. }
        ));

        let second_blocked = engine
            .advance_deterministic_step(&second_id, serde_json::json!("second"), None)
            .unwrap();
        assert!(
            matches!(
                second_blocked,
                SopRunAction::Pending { step: 2, ref reason, .. }
                    if reason.contains("pending-approval pool full")
            ),
            "second checkpoint must not park past max_pending_approvals"
        );
        assert_eq!(
            engine.get_run(&second_id).unwrap().status,
            SopRunStatus::Pending
        );

        let skipped = engine
            .advance_step(
                &second_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "unauthorized checkpoint".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .expect_err("pending checkpoint-cap backpressure must not be advanceable");
        assert!(
            skipped.to_string().contains("pending at gated step"),
            "unexpected advance error: {skipped}"
        );
        assert_eq!(
            engine.get_run(&second_id).unwrap().status,
            SopRunStatus::Pending,
            "the capped checkpoint gate remains pending and cannot be bypassed"
        );
        let first_resumed = engine
            .decide_checkpoint(&first_id, ApprovalDecision::Approve)
            .unwrap();
        assert!(matches!(
            first_resumed,
            SopRunAction::DeterministicStep { .. }
        ));

        engine.run_maintenance_tick();
        assert_eq!(
            engine.get_run(&second_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "maintenance retries the blocked checkpoint once pending capacity frees"
        );
        assert_eq!(
            engine.exec_counts("det-cp-cap"),
            (1, 1),
            "the recovered second checkpoint releases its kept claim while paused"
        );

        let second_resumed = engine
            .decide_checkpoint(&second_id, ApprovalDecision::Approve)
            .unwrap();
        assert!(matches!(
            second_resumed,
            SopRunAction::DeterministicStep { .. }
        ));
    }

    #[test]
    fn pending_park_retry_respects_pending_pool_cap() {
        let store = std::sync::Arc::new(FailingSaveLeasedStore::healthy());
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.max_concurrent = 2;
        sop.max_pending_approvals = 1;
        sop.steps[1].requires_confirmation = true;
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        let first = engine.start_run("s1", manual_event()).unwrap();
        let first_id = extract_run_id(&first).to_string();
        let second = engine.start_run("s1", manual_event()).unwrap();
        let second_id = extract_run_id(&second).to_string();
        assert_eq!(store.claim_counts("s1").unwrap(), (2, 2));

        store.fail_next_save();
        let first_gate = engine
            .advance_step(
                &first_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "first".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(
            matches!(first_gate, SopRunAction::Pending { ref reason, .. }
                if reason.contains("park snapshot not yet durably persisted")),
            "failed first park persist must surface as durable pending, got {first_gate:?}"
        );
        assert_eq!(
            engine.get_run(&first_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the in-memory gate remains parked while its claim is kept"
        );
        assert!(engine.is_park_persist_pending(&first_id));

        let second_gate = engine
            .advance_step(
                &second_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "second".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                    tool_calls: Vec::new(),
                },
            )
            .unwrap();
        assert!(matches!(second_gate, SopRunAction::WaitApproval { .. }));
        assert_eq!(
            engine.pending_count_for_sop("s1"),
            1,
            "the second run fills the durable pending pool before retry"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "only the failed first park still holds an exec claim"
        );

        engine.config.approval_timeout_secs = 1;
        engine.active_runs.get_mut(&first_id).unwrap().waiting_since =
            Some("2000-01-01T00:00:00Z".to_string());
        let summary = engine.run_maintenance_tick();
        assert_eq!(
            summary.timed_out, 0,
            "timeout escalation must skip gates whose parked snapshot is still unpersisted"
        );
        assert!(
            summary.timeout_actions.is_empty(),
            "unpersisted parked gates must not produce timeout actions"
        );
        assert_eq!(
            summary.reaped_claims, 0,
            "the kept claim must not be reaped during the blocked retry"
        );
        assert!(
            engine.is_park_persist_pending(&first_id),
            "retry must keep tracking the first run while the pending pool is full"
        );
        assert_eq!(
            engine.pending_count_for_sop("s1"),
            1,
            "maintenance retry must not persist the first gate past the pending cap"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the first run's claim remains held until its parked snapshot can persist"
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
        engine_a
            .finish_run(&run_id, SopRunStatus::Completed, None)
            .unwrap();

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

    // ── Advance step gate guard ─────────────────────────────
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
    fn timeout_cancel_terminal_failure_does_not_write_timeout_event() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
        });
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            approval_timeout_action: zeroclaw_config::schema::ApprovalTimeoutAction::Cancel,
            ..SopConfig::default()
        })
        .with_store(store.clone());
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine.active_runs.get_mut(&run_id).unwrap().waiting_since =
            Some("2020-01-01T00:00:00Z".into());

        store
            .fail_finish
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let actions = engine.check_approval_timeouts();

        assert!(
            actions.is_empty(),
            "failed cancel persistence retries later"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the gate stays waiting when terminal persistence fails"
        );
        assert!(
            !engine
                .run_events(&run_id)
                .unwrap()
                .iter()
                .any(|ev| ev.kind == "gate_timed_out"),
            "timeout cancel must not write a ledger row without terminal state"
        );
    }

    #[test]
    fn timeout_escalate_save_failure_does_not_write_escalation_event() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
        });
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            ..SopConfig::default()
        })
        .with_store(store.clone());
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        let overdue = "2020-01-01T00:00:00Z".to_string();
        engine.active_runs.get_mut(&run_id).unwrap().waiting_since = Some(overdue.clone());

        store
            .fail_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let actions = engine.check_approval_timeouts();

        assert!(
            actions.is_empty(),
            "failed escalation persistence retries later"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the gate stays waiting when restamp persistence fails"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().waiting_since.as_deref(),
            Some(overdue.as_str()),
            "failed escalation persistence rolls back the in-memory restamp"
        );
        assert!(
            !engine
                .run_events(&run_id)
                .unwrap()
                .iter()
                .any(|ev| ev.kind == "gate_escalated"),
            "timeout escalate must not write a ledger row without the restamp"
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
    fn resume_admission_enforces_per_sop_concurrency_cap() {
        // Reviewer scenario: with `max_concurrent = 1` and the default unbounded pending
        // pool, many runs can park (each releasing its slot), then approving them all must
        // NOT let them all resume at once. Capped resume: the first resumes; the rest are
        // refused at capacity (`DeferredAtCapacity`) and stay parked, re-resolvable.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        sop.max_concurrent = 1;
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        // Two runs park in sequence (the first frees its slot on park, so the second admits).
        let a = engine.start_run("s1", manual_event()).unwrap();
        let id_a = extract_run_id(&a).to_string();
        assert!(
            matches!(a, SopRunAction::WaitApproval { .. }),
            "run A parks: {a:?}"
        );
        let b = engine.start_run("s1", manual_event()).unwrap();
        let id_b = extract_run_id(&b).to_string();
        assert!(
            matches!(b, SopRunAction::WaitApproval { .. }),
            "run B parks too: {b:?}"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "both parked: no exec claim held"
        );

        // Approve A: it resumes into the single free slot.
        let out_a = engine
            .resolve_gate(
                &id_a,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert!(out_a.is_resumed(), "A resumes: {out_a:?}");
        assert_eq!(
            store.claim_counts("s1").unwrap().0,
            1,
            "A holds the one exec slot"
        );

        // Approve B: the slot is taken, so B must defer at capacity - never oversubscribe.
        let out_b = engine
            .resolve_gate(
                &id_b,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert!(
            matches!(out_b, ResolveOutcome::DeferredAtCapacity),
            "B is refused at capacity, not oversubscribed: {out_b:?}"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap().0,
            1,
            "still exactly one exec slot in use, not two"
        );
        assert!(
            matches!(engine.gate_state(&id_b), GateState::Waiting { .. }),
            "B stays WaitingApproval, re-resolvable"
        );
    }

    #[test]
    fn resume_admission_enforces_global_concurrency_cap() {
        // The global `max_concurrent_total` is enforced on resume too: two DIFFERENT SOPs
        // (each `max_concurrent = 1`) share a global cap of 1. Both park; approving both
        // resumes only the first - the second defers at capacity.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut s1 = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        s1.max_concurrent = 1;
        let mut s2 = test_sop("s2", SopExecutionMode::Supervised, SopPriority::Normal);
        s2.max_concurrent = 1;
        let cfg = SopConfig {
            max_concurrent_total: 1,
            ..SopConfig::default()
        };
        let mut engine = engine_with_config_sops(cfg, vec![s1, s2]).with_store(store.clone());

        let a = engine.start_run("s1", manual_event()).unwrap();
        let id_a = extract_run_id(&a).to_string();
        let b = engine.start_run("s2", manual_event()).unwrap();
        let id_b = extract_run_id(&b).to_string();
        assert!(
            matches!(a, SopRunAction::WaitApproval { .. })
                && matches!(b, SopRunAction::WaitApproval { .. }),
            "both runs park for approval"
        );

        let out_a = engine
            .resolve_gate(
                &id_a,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert!(
            out_a.is_resumed(),
            "the first resumes into the one global slot"
        );
        let out_b = engine
            .resolve_gate(
                &id_b,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(None),
            )
            .unwrap();
        assert!(
            matches!(out_b, ResolveOutcome::DeferredAtCapacity),
            "the global cap refuses the second resume: {out_b:?}"
        );
        assert_eq!(
            store.claim_counts("s2").unwrap().1,
            1,
            "exactly one exec slot in use globally, not two"
        );
    }

    #[test]
    fn checkpoint_resume_enforces_concurrency_cap() {
        // The cap applies to the checkpoint-resume path (`approve_step`) too, via the same
        // reacquire chokepoint. Two deterministic runs park at a checkpoint (each frees its
        // slot); approving both resumes only the first - the second is refused at capacity
        // with the typed backpressure marker, and stays paused.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = deterministic_sop("det-cp");
        sop.max_concurrent = 1;
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        let a = engine.start_run("det-cp", manual_event()).unwrap();
        let id_a = extract_run_id(&a).to_string();
        engine
            .advance_deterministic_step(&id_a, serde_json::json!("a1"), None)
            .unwrap();
        assert_eq!(
            engine.get_run(&id_a).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );
        let b = engine.start_run("det-cp", manual_event()).unwrap();
        let id_b = extract_run_id(&b).to_string();
        engine
            .advance_deterministic_step(&id_b, serde_json::json!("b1"), None)
            .unwrap();
        assert_eq!(
            engine.get_run(&id_b).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );
        assert_eq!(
            store.claim_counts("det-cp").unwrap(),
            (0, 0),
            "both parked at the checkpoint: no exec claim held"
        );

        engine.approve_step(&id_a).unwrap();
        assert_eq!(
            store.claim_counts("det-cp").unwrap().0,
            1,
            "A holds the one slot after resuming"
        );

        let err = engine
            .approve_step(&id_b)
            .expect_err("B's checkpoint resume must be refused at capacity");
        assert!(
            err_is_resume_at_capacity(&err),
            "the refusal is typed capacity backpressure, not a fault: {err}"
        );
        assert_eq!(
            engine.get_run(&id_b).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "B stays paused at the checkpoint, re-resolvable"
        );
        assert_eq!(
            store.claim_counts("det-cp").unwrap().0,
            1,
            "still exactly one slot in use, not two"
        );
    }

    #[test]
    fn sqlite_daemon_restart_resumes_parked_run_and_enforces_cap() {
        // Near-live boundary evidence: with a REAL file-backed SQLite store, runs parked for
        // approval survive a daemon "restart" (a fresh engine over the same DB), restore
        // holding no exec slot, and the resume concurrency cap holds ACROSS the restart -
        // exercising the durable status round-trip plus capped `reacquire_claim_on_resume`.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sop.db");

        // Boot 1: park two runs of a max_concurrent=1 SOP, then shut down.
        let (id_a, id_b);
        {
            let store =
                std::sync::Arc::new(crate::sop::store::sqlite::SqliteRunStore::open(&db).unwrap());
            let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
            sop.max_concurrent = 1;
            let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
            let a = engine.start_run("s1", manual_event()).unwrap();
            id_a = extract_run_id(&a).to_string();
            let b = engine.start_run("s1", manual_event()).unwrap();
            id_b = extract_run_id(&b).to_string();
            assert!(
                matches!(a, SopRunAction::WaitApproval { .. })
                    && matches!(b, SopRunAction::WaitApproval { .. }),
                "both runs park for approval"
            );
            assert_eq!(
                store.claim_counts("s1").unwrap(),
                (0, 0),
                "parked runs hold no exec slot (durably)"
            );
        }

        // Boot 2: restart over the SAME DB, restore, then approve both.
        {
            let store =
                std::sync::Arc::new(crate::sop::store::sqlite::SqliteRunStore::open(&db).unwrap());
            let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
            sop.max_concurrent = 1;
            let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
            engine.restore_runs();
            assert_eq!(
                engine.get_run(&id_a).map(|r| r.status),
                Some(SopRunStatus::WaitingApproval),
                "run A restored WaitingApproval after restart"
            );
            assert_eq!(
                engine.get_run(&id_b).map(|r| r.status),
                Some(SopRunStatus::WaitingApproval),
                "run B restored WaitingApproval after restart"
            );
            assert_eq!(
                store.claim_counts("s1").unwrap(),
                (0, 0),
                "restored parked runs hold no exec claim"
            );

            // Approve A: resumes into the free slot. Approve B: refused at capacity - the cap
            // holds across the restart boundary.
            let out_a = engine
                .resolve_gate(
                    &id_a,
                    ApprovalDecision::Approve,
                    ApprovalPrincipal::cli(None),
                )
                .unwrap();
            assert!(out_a.is_resumed(), "A resumes after restart: {out_a:?}");
            let out_b = engine
                .resolve_gate(
                    &id_b,
                    ApprovalDecision::Approve,
                    ApprovalPrincipal::cli(None),
                )
                .unwrap();
            assert!(
                matches!(out_b, ResolveOutcome::DeferredAtCapacity),
                "the resume cap holds across restart: B is refused at capacity: {out_b:?}"
            );
            assert_eq!(
                store.claim_counts("s1").unwrap().0,
                1,
                "exactly one exec slot in use after restart + resume, not two"
            );
        }
    }

    #[test]
    fn rollback_activated_run_durably_cancels_a_parked_sibling() {
        // 2b atomic-rollback: a sibling that PARKED (persisted) during activation and is then
        // rolled back (because a later sibling failed to activate) must be durably CANCELLED,
        // not merely dropped in memory - otherwise `restore_runs` reconstructs an orphaned
        // parked run after a restart, duplicating a delivery that was deferred + requeued.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )])
        .with_store(store.clone());
        // A sibling that activated and PARKED at its step-1 approval gate (persisted).
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
        assert!(
            store
                .load_active_runs()
                .unwrap()
                .iter()
                .any(|r| r.run.run_id == run_id),
            "the parked sibling is durable before rollback"
        );

        // Roll it back, as the atomic batch does when a later sibling's activation fails.
        engine.rollback_activated_run(&run_id);
        assert!(
            engine.get_run(&run_id).is_none(),
            "the rolled-back sibling is dropped in memory"
        );
        // The durable row is now terminal Cancelled, not an active parked run.
        assert!(
            store
                .load_active_runs()
                .unwrap()
                .iter()
                .all(|r| r.run.run_id != run_id),
            "the rolled-back parked sibling is no longer a durable ACTIVE run"
        );

        // A restart must NOT resurrect it as a LIVE parked run (the post-requeue duplicate);
        // at most it appears as terminal history.
        let mut fresh = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )])
        .with_store(store.clone());
        fresh.restore_runs();
        let restored = fresh.get_run(&run_id).map(|r| r.status);
        assert!(
            restored.is_none() || restored == Some(SopRunStatus::Cancelled),
            "restart must not resurrect the rolled-back sibling as a live parked run (got {restored:?})"
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
    fn restore_fails_closed_when_retention_inspection_errors() {
        // Finding 3: if inspecting the terminal-rollback retention marker ERRORS during
        // restore, we must fail CLOSED and KEEP the claim - a transient read failure must
        // not discard a claim the marker exists to preserve. (The prior code mapped the
        // error to `retained = false`, routing a legitimate marker into the release branch.)
        let store = std::sync::Arc::new(FailingSaveLeasedStore::healthy());
        // Seed a parked run whose current step has NO recorded result (a legitimate,
        // non-stale terminal-rollback marker) plus a retained claim for it.
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
        };
        store
            .save_run(&PersistedRun::new(parked, now, SopTriggerSource::Manual))
            .unwrap();
        store.try_claim_run("parked-1", "s1", 1, 4).unwrap();
        store
            .mark_claim_retained_after_terminal_rollback("parked-1")
            .unwrap();
        assert_eq!(
            store.claim_counts("s1").unwrap().1,
            1,
            "seeded a retained terminal-rollback claim"
        );

        // Make the retention inspection fail during restore.
        store.set_fail_has_retained(true);
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        engine.restore_runs();

        // Fail-closed: the claim is PRESERVED, not discarded, and the run is still restored.
        assert_eq!(
            store.claim_counts("s1").unwrap().1,
            1,
            "an inspection error must fail closed: the retained claim survives (not released)"
        );
        assert!(
            engine.get_run("parked-1").is_some(),
            "the parked run is still restored"
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

    /// Delegates to an in-memory store but can be flipped to fail claim acquisition
    /// (both the capped `try_claim_run` the resume reacquire now uses and the uncapped
    /// `renew_claim_for_restore`), to prove resume fails CLOSED when the claim store
    /// errors. Flipped ON only after the initial admit so `start_run` still succeeds.
    struct FailingReacquireStore {
        inner: InMemoryRunStore,
        fail_claim: std::sync::atomic::AtomicBool,
    }
    impl SopRunStore for FailingReacquireStore {
        fn save_run(&self, r: &PersistedRun) -> Result<(), StoreError> {
            self.inner.save_run(r)
        }
        fn save_run_with_event(
            &self,
            r: &PersistedRun,
            e: &SopEventRecord,
        ) -> Result<u64, StoreError> {
            self.inner.save_run_with_event(r, e)
        }
        fn finish_run(&self, id: &str, t: &PersistedRun) -> Result<(), StoreError> {
            self.inner.finish_run(id, t)
        }
        fn finish_run_with_event(
            &self,
            id: &str,
            t: &PersistedRun,
            e: &SopEventRecord,
        ) -> Result<u64, StoreError> {
            self.inner.finish_run_with_event(id, t, e)
        }
        fn load_terminal_runs(
            &self,
            _limit: usize,
        ) -> Result<Vec<crate::sop::store::PersistedRun>, crate::sop::store::StoreError> {
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
            if self.fail_claim.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected claim failure".into()));
            }
            self.inner.try_claim_run(id, s, p, g)
        }
        fn renew_claim_for_restore(&self, id: &str, s: &str) -> Result<ClaimToken, StoreError> {
            if self.fail_claim.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected renew failure".into()));
            }
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
            "failing-reacquire-test"
        }
    }

    #[test]
    fn resume_fails_closed_when_claim_reacquire_fails() {
        // If the claim store errors during resume, the run must NOT execute
        // uncounted: the resume aborts (Err) and the gate stays WaitingApproval.
        let store = std::sync::Arc::new(FailingReacquireStore {
            inner: InMemoryRunStore::new(),
            fail_claim: std::sync::atomic::AtomicBool::new(false),
        });
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let a = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&a).to_string();
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval
        );
        // Fail the claim store now (after the admit): the resume reacquire hits a
        // store fault (not capacity backpressure) and must abort fail-closed.
        store
            .fail_claim
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

    /// Delegates to an in-memory store but can be flipped to fail an audit append
    /// or terminal persistence, exercising both claim-ordering failure paths.
    struct FailingAppendStore {
        inner: InMemoryRunStore,
        fail: std::sync::atomic::AtomicBool,
        fail_save: std::sync::atomic::AtomicBool,
        fail_finish: std::sync::atomic::AtomicBool,
    }
    impl SopRunStore for FailingAppendStore {
        fn save_run(&self, r: &PersistedRun) -> Result<(), StoreError> {
            if self.fail_save.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected save_run failure".into()));
            }
            self.inner.save_run(r)
        }
        fn save_run_with_event(
            &self,
            r: &PersistedRun,
            e: &SopEventRecord,
        ) -> Result<u64, StoreError> {
            if self.fail_save.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected save_run failure".into()));
            }
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected append failure".into()));
            }
            self.inner.save_run_with_event(r, e)
        }
        fn finish_run(&self, id: &str, t: &PersistedRun) -> Result<(), StoreError> {
            if self.fail_finish.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected finish failure".into()));
            }
            self.inner.finish_run(id, t)
        }
        fn finish_run_with_event(
            &self,
            id: &str,
            t: &PersistedRun,
            e: &SopEventRecord,
        ) -> Result<u64, StoreError> {
            if self.fail_finish.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected finish failure".into()));
            }
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected append failure".into()));
            }
            self.inner.finish_run_with_event(id, t, e)
        }
        fn load_terminal_runs(
            &self,
            _limit: usize,
        ) -> Result<Vec<crate::sop::store::PersistedRun>, crate::sop::store::StoreError> {
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
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
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

    #[test]
    fn approval_active_persist_failure_rolls_back_transition_and_ledger() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
        });
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "the gate must be durably parked before this test flips save_run failures on"
        );

        store
            .fail_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err = engine
            .resolve_gate(
                &run_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .expect_err("active transition persistence failure must reject approval");
        assert!(err.to_string().contains("injected save_run failure"));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "failed active persistence must roll the in-memory gate back to waiting"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (0, 0),
            "the claim reacquired for the rejected approval must be released"
        );
        let events = engine.run_events(&run_id).unwrap_or_default();
        assert!(
            !events.iter().any(|ev| ev.kind == "gate_resolved"),
            "a failed active transition must not append a gate_resolved row: {events:?}"
        );
    }

    #[test]
    fn approval_schema_reject_failure_rolls_back_without_partial_terminal_state() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
        });
        let sop = test_sop(
            "schema-gate",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        );
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: Some("{}".into()),
            timestamp: now_iso8601(),
        };
        let action = engine.start_run("schema-gate", event).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval
        );
        let mut tightened = test_sop(
            "schema-gate",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        );
        tightened.steps[0].schema = Some(StepSchema {
            input: Some(required_object_schema("ok")),
            output: None,
        });
        engine.set_sops_for_test(vec![tightened]);

        store
            .fail_finish
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err = engine
            .resolve_gate(
                &run_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .expect_err("terminal schema-reject commit failure must reject approval");
        assert!(err.to_string().contains("injected finish failure"));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "failed terminal persistence must restore the in-memory gate"
        );
        assert!(
            engine.finished_runs(None).is_empty(),
            "failed approval must not push a terminal run into the cache"
        );
        assert_eq!(
            store.load_run(&run_id).unwrap().unwrap().run.status,
            SopRunStatus::WaitingApproval,
            "durable state must remain the parked gate"
        );
        assert_eq!(
            store.claim_counts("schema-gate").unwrap(),
            (0, 0),
            "the reacquired claim must be released after the rejected approval"
        );
        let events = store.list_events(&run_id).unwrap();
        assert!(
            !events.iter().any(|ev| ev.kind == "gate_resolved"),
            "the rejected approval must not append gate_resolved: {events:?}"
        );
        assert!(
            !events.iter().any(|ev| ev.kind == "step_schema_reject"),
            "secondary schema events must wait for the terminal gate commit: {events:?}"
        );
    }

    #[test]
    fn approval_route_pending_failure_rolls_back_without_step_skipped_event() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
        });
        let sop = test_sop(
            "route-gate",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        );
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine.start_run("route-gate", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let mut changed = test_sop(
            "route-gate",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        );
        changed.steps[0].routing.depends_on = vec![42];
        engine.set_sops_for_test(vec![changed]);
        store
            .fail_save
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let err = engine
            .resolve_gate(
                &run_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::cli(Some("alice".into())),
            )
            .expect_err("route-ineligible active commit failure must reject approval");
        assert!(err.to_string().contains("injected save_run failure"));
        let run = engine.get_run(&run_id).unwrap();
        assert_eq!(
            run.status,
            SopRunStatus::WaitingApproval,
            "failed pending persistence must restore the in-memory gate"
        );
        assert!(
            run.step_results.is_empty(),
            "pending skipped step must roll back with the gate"
        );
        assert_eq!(
            store.load_run(&run_id).unwrap().unwrap().run.status,
            SopRunStatus::WaitingApproval,
            "durable state must remain the parked gate"
        );
        assert_eq!(
            store.claim_counts("route-gate").unwrap(),
            (0, 0),
            "the reacquired claim must be released after the rejected approval"
        );
        assert!(
            engine.finished_runs(None).is_empty(),
            "route-ineligible active failure must not create terminal cache entries"
        );
        let events = store.list_events(&run_id).unwrap();
        assert!(
            !events.iter().any(|ev| ev.kind == "gate_resolved"),
            "the rejected approval must not append gate_resolved: {events:?}"
        );
        assert!(
            !events.iter().any(|ev| ev.kind == "step_skipped"),
            "secondary pending events must wait for the active gate commit: {events:?}"
        );
    }

    /// Delegates to an in-memory store but fails every `save_run`, to prove a park
    /// does NOT release its exec claim when the parked snapshot cannot be durably
    /// persisted.
    struct FailingSaveStore {
        inner: InMemoryRunStore,
    }
    impl SopRunStore for FailingSaveStore {
        fn save_run(&self, _r: &PersistedRun) -> Result<(), StoreError> {
            Err(StoreError::Backend("injected save_run failure".into()))
        }
        fn save_run_with_event(
            &self,
            _r: &PersistedRun,
            _e: &SopEventRecord,
        ) -> Result<u64, StoreError> {
            Err(StoreError::Backend("injected save_run failure".into()))
        }
        fn finish_run(&self, id: &str, t: &PersistedRun) -> Result<(), StoreError> {
            self.inner.finish_run(id, t)
        }
        fn finish_run_with_event(
            &self,
            id: &str,
            t: &PersistedRun,
            e: &SopEventRecord,
        ) -> Result<u64, StoreError> {
            self.inner.finish_run_with_event(id, t, e)
        }
        fn load_terminal_runs(
            &self,
            _limit: usize,
        ) -> Result<Vec<crate::sop::store::PersistedRun>, crate::sop::store::StoreError> {
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
        let store = std::sync::Arc::new(FailingSaveStore {
            inner: InMemoryRunStore::new(),
        });
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let a = engine.start_run("s1", manual_event()).unwrap();
        assert!(
            matches!(
                a,
                SopRunAction::Pending {
                    step: 1,
                    ref reason,
                    ..
                } if reason.contains("park snapshot not yet durably persisted")
            ),
            "a supervised first step reports durable pending while keeping its claim, got {a:?}"
        );
        let run_id = extract_run_id(&a).to_string();
        assert!(
            engine.is_park_persist_pending(&run_id),
            "the failed park persist must be tracked until a later retry succeeds"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the canonical run must stay parked while the transient action reports Pending"
        );
        let advance = engine.advance_step(
            &run_id,
            SopStepResult {
                step_number: 1,
                status: SopStepStatus::Completed,
                output: "should not advance".into(),
                started_at: now_iso8601(),
                completed_at: Some(now_iso8601()),
                tool_calls: Vec::new(),
            },
        );
        assert!(
            advance.is_err(),
            "sop_advance must not bypass an approval gate whose park snapshot is still pending"
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
    fn checkpoint_park_keeps_its_claim_when_the_snapshot_persist_fails() {
        // Same fail-closed guarantee as the approval-park case, for the
        // deterministic-checkpoint park site.
        let store = std::sync::Arc::new(FailingSaveStore {
            inner: InMemoryRunStore::new(),
        });
        let mut engine =
            engine_with_sops(vec![deterministic_sop("det-cp")]).with_store(store.clone());
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(
            matches!(
                action,
                SopRunAction::Pending {
                    step: 2,
                    ref reason,
                    ..
                } if reason.contains("park snapshot not yet durably persisted")
            ),
            "a checkpoint park reports durable pending while keeping its claim, got {action:?}"
        );
        assert!(
            engine.is_park_persist_pending(&run_id),
            "the failed checkpoint persist must be tracked until a later retry succeeds"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "the canonical run must stay parked while the transient action reports Pending"
        );
        let advance = engine.advance_step(
            &run_id,
            SopStepResult {
                step_number: 2,
                status: SopStepStatus::Completed,
                output: "should not advance".into(),
                started_at: now_iso8601(),
                completed_at: Some(now_iso8601()),
                tool_calls: Vec::new(),
            },
        );
        assert!(
            advance.is_err(),
            "sop_advance must not bypass a checkpoint whose park snapshot is still pending"
        );
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
        // A failed park persist keeps the exec claim and downgrades the exposed
        // action to Pending, because there is no durably parked approval row to
        // resolve yet. Any manual approval attempt must fail without releasing
        // the pre-existing kept claim.
        let store = std::sync::Arc::new(FailingSaveStore {
            inner: InMemoryRunStore::new(),
        });
        let sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Normal);
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let a = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&a).to_string();
        assert!(
            engine.is_park_persist_pending(&run_id),
            "the failed park persist must be tracked while the claim is kept"
        );
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
            "approval must not resume while the park's snapshot is not yet durably persisted"
        );
        assert_eq!(
            store.claim_counts("s1").unwrap(),
            (1, 1),
            "the pre-existing kept claim must survive the refused approval attempt"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval,
            "the run stays parked, re-resolvable once the park persists"
        );
    }

    #[test]
    fn approve_step_refuses_to_resume_while_checkpoint_persist_is_pending() {
        // Same class of regression as the approval park case, for the
        // deterministic-checkpoint resume path.
        let store = std::sync::Arc::new(FailingSaveStore {
            inner: InMemoryRunStore::new(),
        });
        let mut engine =
            engine_with_sops(vec![deterministic_sop("det-cp")]).with_store(store.clone());
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(
            matches!(
                action,
                SopRunAction::Pending {
                    step: 2,
                    ref reason,
                    ..
                } if reason.contains("park snapshot not yet durably persisted")
            ),
            "the failed checkpoint persist must surface as durable pending, got {action:?}"
        );
        assert!(
            engine.is_park_persist_pending(&run_id),
            "the failed checkpoint persist must be tracked while the claim is kept"
        );
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
        let store = std::sync::Arc::new(FailingSaveStore {
            inner: InMemoryRunStore::new(),
        });
        let mut engine =
            engine_with_sops(vec![deterministic_sop("det-cp")]).with_store(store.clone());
        let action = engine.start_run("det-cp", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(
            matches!(
                action,
                SopRunAction::Pending {
                    step: 2,
                    ref reason,
                    ..
                } if reason.contains("park snapshot not yet durably persisted")
            ),
            "the failed checkpoint persist must surface as durable pending, got {action:?}"
        );
        assert!(
            engine.is_park_persist_pending(&run_id),
            "the failed checkpoint persist must be tracked while the claim is kept"
        );
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
    /// lease. Can inject either `save_run` or terminal `finish_run` failures while
    /// keeping real expiring claims, so maintenance tests can prove retained
    /// claims are renewed rather than reaped.
    struct FailingSaveLeasedStore {
        inner: InMemoryRunStore,
        claims: std::sync::Mutex<std::collections::HashMap<String, ClaimToken>>,
        fail_save: std::sync::atomic::AtomicBool,
        fail_next_save: std::sync::atomic::AtomicBool,
        fail_finish: std::sync::atomic::AtomicBool,
        fail_marker: std::sync::atomic::AtomicBool,
        fail_release: std::sync::atomic::AtomicBool,
        fail_has_retained: std::sync::atomic::AtomicBool,
    }
    impl FailingSaveLeasedStore {
        fn healthy() -> Self {
            Self {
                inner: InMemoryRunStore::new(),
                claims: std::sync::Mutex::new(std::collections::HashMap::new()),
                fail_save: std::sync::atomic::AtomicBool::new(false),
                fail_next_save: std::sync::atomic::AtomicBool::new(false),
                fail_finish: std::sync::atomic::AtomicBool::new(false),
                fail_marker: std::sync::atomic::AtomicBool::new(false),
                fail_release: std::sync::atomic::AtomicBool::new(false),
                fail_has_retained: std::sync::atomic::AtomicBool::new(false),
            }
        }
        fn new() -> Self {
            Self {
                inner: InMemoryRunStore::new(),
                claims: std::sync::Mutex::new(std::collections::HashMap::new()),
                fail_save: std::sync::atomic::AtomicBool::new(true),
                fail_next_save: std::sync::atomic::AtomicBool::new(false),
                fail_finish: std::sync::atomic::AtomicBool::new(false),
                fail_marker: std::sync::atomic::AtomicBool::new(false),
                fail_release: std::sync::atomic::AtomicBool::new(false),
                fail_has_retained: std::sync::atomic::AtomicBool::new(false),
            }
        }
        fn finish_fails() -> Self {
            Self {
                inner: InMemoryRunStore::new(),
                claims: std::sync::Mutex::new(std::collections::HashMap::new()),
                fail_save: std::sync::atomic::AtomicBool::new(false),
                fail_next_save: std::sync::atomic::AtomicBool::new(false),
                fail_finish: std::sync::atomic::AtomicBool::new(true),
                fail_marker: std::sync::atomic::AtomicBool::new(false),
                fail_release: std::sync::atomic::AtomicBool::new(false),
                fail_has_retained: std::sync::atomic::AtomicBool::new(false),
            }
        }
        fn finish_and_marker_fail() -> Self {
            Self {
                inner: InMemoryRunStore::new(),
                claims: std::sync::Mutex::new(std::collections::HashMap::new()),
                fail_save: std::sync::atomic::AtomicBool::new(false),
                fail_next_save: std::sync::atomic::AtomicBool::new(false),
                fail_finish: std::sync::atomic::AtomicBool::new(true),
                fail_marker: std::sync::atomic::AtomicBool::new(true),
                fail_release: std::sync::atomic::AtomicBool::new(false),
                fail_has_retained: std::sync::atomic::AtomicBool::new(false),
            }
        }
        fn fail_next_save(&self) {
            self.fail_next_save
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        /// Inject a claim-release failure: the next (and subsequent) `release_claim`
        /// calls error AND leave the claim row in place, modelling a transient store
        /// fault during the checkpoint-denial continuation release.
        fn set_fail_release(&self, on: bool) {
            self.fail_release
                .store(on, std::sync::atomic::Ordering::SeqCst);
        }
        /// Inject a retention-marker inspection failure: `has_retained_terminal_rollback_claim`
        /// errors, modelling a transient claim-store read fault during restore.
        fn set_fail_has_retained(&self, on: bool) {
            self.fail_has_retained
                .store(on, std::sync::atomic::Ordering::SeqCst);
        }
        fn should_fail_save(&self) -> bool {
            self.fail_save.load(std::sync::atomic::Ordering::SeqCst)
                || self
                    .fail_next_save
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
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
        fn save_run(&self, r: &PersistedRun) -> Result<(), StoreError> {
            if self.should_fail_save() {
                Err(StoreError::Backend("injected save_run failure".into()))
            } else {
                self.inner.save_run(r)
            }
        }
        fn save_run_with_event(
            &self,
            r: &PersistedRun,
            e: &SopEventRecord,
        ) -> Result<u64, StoreError> {
            if self.should_fail_save() {
                Err(StoreError::Backend("injected save_run failure".into()))
            } else {
                self.inner.save_run_with_event(r, e)
            }
        }
        fn finish_run(&self, id: &str, t: &PersistedRun) -> Result<(), StoreError> {
            if self.fail_finish.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected finish failure".into()));
            }
            self.inner.finish_run(id, t)?;
            self.claims.lock().unwrap().remove(id);
            Ok(())
        }
        fn finish_run_with_event(
            &self,
            id: &str,
            t: &PersistedRun,
            e: &SopEventRecord,
        ) -> Result<u64, StoreError> {
            if self.fail_finish.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected finish failure".into()));
            }
            let seq = self.inner.finish_run_with_event(id, t, e)?;
            self.claims.lock().unwrap().remove(id);
            Ok(seq)
        }
        fn load_terminal_runs(
            &self,
            _limit: usize,
        ) -> Result<Vec<crate::sop::store::PersistedRun>, crate::sop::store::StoreError> {
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
        fn mark_claim_retained_after_terminal_rollback(
            &self,
            run_id: &str,
        ) -> Result<(), StoreError> {
            if self.fail_marker.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Backend("injected marker failure".into()));
            }
            if let Some(token) = self.claims.lock().unwrap().get_mut(run_id) {
                token.holder = crate::sop::store::RETAINED_TERMINAL_ROLLBACK_HOLDER.to_string();
            }
            Ok(())
        }
        fn has_retained_terminal_rollback_claim(&self, run_id: &str) -> Result<bool, StoreError> {
            if self
                .fail_has_retained
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StoreError::Backend(
                    "injected retention-marker inspection failure".into(),
                ));
            }
            Ok(self
                .claims
                .lock()
                .unwrap()
                .get(run_id)
                .is_some_and(|token| {
                    token.holder == crate::sop::store::RETAINED_TERMINAL_ROLLBACK_HOLDER
                }))
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
            if self.fail_release.load(std::sync::atomic::Ordering::SeqCst) {
                // Model a transient store fault: the claim row survives the failed
                // release so a swallowed failure would leak it.
                return Err(StoreError::Backend("injected release failure".into()));
            }
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
        // Keeping the claim on a failed park
        // persist is only fail-closed if the kept claim's lease keeps being
        // renewed. Without tracking it in `claims_pending_persist`,
        // `heartbeat_active_claims` skips it (parked status), its lease goes
        // un-renewed, and `reap_expired_claims` reclaims it once the lease is in
        // the past - silently undoing the fail-closed keep and letting a newer
        // trigger over-admit.
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
    fn checkpoint_state_file_failure_keeps_run_executing_and_claim_renewed() {
        let store = std::sync::Arc::new(FailingSaveLeasedStore::healthy());
        let mut sop = deterministic_sop("det-cp-state-file-fails");
        let location_file = std::env::temp_dir().join(format!(
            "zc-state-location-file-{}",
            now_iso8601().replace(':', "-")
        ));
        std::fs::write(&location_file, "not a directory").unwrap();
        sop.location = Some(location_file.clone());

        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-state-file-fails", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();

        let err = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .expect_err("checkpoint state-file write must fail for a file-valued location");
        assert!(
            err.to_string().contains("Not a directory")
                || err.to_string().contains("not a directory"),
            "unexpected state-file error: {err}"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Running,
            "state-file failure must not park the run before the checkpoint is durable"
        );
        assert!(
            !engine.is_park_persist_pending(&run_id),
            "state-file failure happens before park-persist retry tracking is needed"
        );
        assert_eq!(
            store.claim_counts("det-cp-state-file-fails").unwrap(),
            (1, 1),
            "the still-running run keeps its execution claim"
        );

        store.expire_claim_now(&run_id);
        let summary = engine.run_maintenance_tick();
        assert_eq!(
            summary.reaped_claims, 0,
            "maintenance must heartbeat the still-running claim before reaping"
        );
        assert_eq!(
            store.claim_counts("det-cp-state-file-fails").unwrap(),
            (1, 1),
            "the execution claim remains live after maintenance"
        );

        let _ = std::fs::remove_file(location_file);
    }

    #[test]
    fn denied_checkpoint_terminal_rollback_claim_survives_restart_and_maintenance_reap() {
        let store = std::sync::Arc::new(FailingSaveLeasedStore::finish_fails());
        let mut sop = deterministic_sop("det-cp-deny-finish-lease");
        sop.max_concurrent = 1;
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-finish-lease", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();

        let checkpoint = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(checkpoint, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            store.claim_counts("det-cp-deny-finish-lease").unwrap(),
            (0, 0),
            "a durably parked checkpoint starts without an execution claim"
        );

        let err = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect_err("terminal persistence failure must reject the denial");
        assert!(err.to_string().contains("injected finish failure"));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint
        );
        assert_eq!(
            store.claim_counts("det-cp-deny-finish-lease").unwrap(),
            (1, 1),
            "the failed terminal write keeps the reacquired claim fail-closed"
        );

        let mut restored_sop = deterministic_sop("det-cp-deny-finish-lease");
        restored_sop.max_concurrent = 1;
        let mut restored = engine_with_sops(vec![restored_sop]).with_store(store.clone());
        restored.restore_runs();
        assert_eq!(
            restored.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "restart must restore the parked checkpoint run"
        );
        assert_eq!(
            store.claim_counts("det-cp-deny-finish-lease").unwrap(),
            (1, 1),
            "restore must preserve the retained terminal-rollback claim"
        );
        assert!(
            !restored.can_start("det-cp-deny-finish-lease"),
            "the retained claim must still block duplicate admission after restart"
        );

        store.expire_claim_now(&run_id);
        let summary = restored.run_maintenance_tick();

        assert_eq!(
            summary.reaped_claims, 0,
            "maintenance must heartbeat the retained terminal-rollback claim before reaping"
        );
        assert_eq!(
            store.claim_counts("det-cp-deny-finish-lease").unwrap(),
            (1, 1),
            "the retained checkpoint-denial claim must survive an expired-lease sweep"
        );
        assert!(
            !restored.can_start("det-cp-deny-finish-lease"),
            "the retained claim must keep the execution slot held until the denial is retried"
        );
    }

    #[test]
    fn denied_checkpoint_marker_failure_aborts_without_retained_claim() {
        let store = std::sync::Arc::new(FailingSaveLeasedStore::finish_and_marker_fail());
        let mut sop = deterministic_sop("det-cp-deny-marker-fail");
        sop.max_concurrent = 1;
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-marker-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();

        let checkpoint = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(checkpoint, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            store.claim_counts("det-cp-deny-marker-fail").unwrap(),
            (0, 0),
            "a durably parked checkpoint starts without an execution claim"
        );

        let err = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect_err("marker persistence failure must reject the denial before terminal write");
        assert!(err.to_string().contains("injected marker failure"));
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "marker failure leaves the checkpoint parked and re-resolvable"
        );
        assert!(
            !store.has_retained_terminal_rollback_claim(&run_id).unwrap(),
            "the injected marker failure must leave no durable marker"
        );
        assert_eq!(
            store.claim_counts("det-cp-deny-marker-fail").unwrap(),
            (0, 0),
            "marker failure releases the reacquired claim instead of retaining it without a marker"
        );

        let mut restored_sop = deterministic_sop("det-cp-deny-marker-fail");
        restored_sop.max_concurrent = 1;
        let mut restored = engine_with_sops(vec![restored_sop]).with_store(store.clone());
        restored.restore_runs();
        assert_eq!(
            restored.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "restart must restore the parked checkpoint run normally"
        );
        assert_eq!(
            store.claim_counts("det-cp-deny-marker-fail").unwrap(),
            (0, 0),
            "restore must not invent retention for an unmarked parked checkpoint"
        );
        assert!(
            restored.can_start("det-cp-deny-marker-fail"),
            "an unmarked parked checkpoint must not consume the execution slot after restart"
        );
    }

    #[test]
    fn denied_checkpoint_goto_checkpoint_releases_claim_after_recovered_park_persist() {
        let store = std::sync::Arc::new(FailingSaveLeasedStore::healthy());
        let mut sop = deterministic_sop("det-cp-deny-goto-cp");
        sop.steps[1].on_failure = StepFailure::Goto { step: 4 };
        sop.steps.push(SopStep {
            number: 4,
            title: "Second checkpoint".into(),
            body: "Pause again".into(),
            suggested_tools: vec![],
            requires_confirmation: false,
            kind: SopStepKind::Checkpoint,
            schema: None,
            ..SopStep::default()
        });
        sop.max_concurrent = 1;

        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-goto-cp", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        let checkpoint = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(checkpoint, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            store.claim_counts("det-cp-deny-goto-cp").unwrap(),
            (0, 0),
            "a durably parked checkpoint starts without an execution claim"
        );

        store.fail_next_save();
        let action = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect("denial should route to the second checkpoint");
        assert!(
            matches!(
                action,
                SopRunAction::Pending {
                    step: 4,
                    ref reason,
                    ..
                } if reason.contains("park snapshot not yet durably persisted")
            ),
            "the first park save failure is still surfaced to the caller, got {action:?}"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "the routed denial ends parked at the second checkpoint"
        );
        assert!(
            !engine.is_park_persist_pending(&run_id),
            "the outer denial persist completed the parked snapshot and must clear retry tracking"
        );
        assert_eq!(
            store.claim_counts("det-cp-deny-goto-cp").unwrap(),
            (0, 0),
            "the outer denial persist must release the exec claim for the parked route target"
        );
        assert!(
            engine.can_start("det-cp-deny-goto-cp"),
            "the parked route target must not consume the SOP concurrency slot"
        );
    }

    #[test]
    fn deny_checkpoint_goto_continuation_release_failure_aborts_without_pinning_slot() {
        // A denied checkpoint whose failure route (Goto) lands on ANOTHER
        // checkpoint CONTINUES the run — it did not terminal-rollback. If clearing the
        // stale terminal-rollback retention marker (the parked-continuation claim
        // release) fails, the denial must NOT return Ok with a live durable marker on a
        // continued run: it fails closed (rolls back + surfaces the error) and drops the
        // in-memory retention so the lease reaper frees the slot instead of the engine
        // renewing it forever.
        let store = std::sync::Arc::new(FailingSaveLeasedStore::healthy());
        let mut sop = deterministic_sop("det-cp-deny-goto-release-fail");
        sop.steps[1].on_failure = StepFailure::Goto { step: 4 };
        sop.steps.push(SopStep {
            number: 4,
            title: "Second checkpoint".into(),
            body: "Pause again".into(),
            suggested_tools: vec![],
            requires_confirmation: false,
            kind: SopStepKind::Checkpoint,
            schema: None,
            ..SopStep::default()
        });
        sop.max_concurrent = 1;

        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-goto-release-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        let checkpoint = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(checkpoint, SopRunAction::CheckpointWait { .. }));
        assert_eq!(
            store.claim_counts("det-cp-deny-goto-release-fail").unwrap(),
            (0, 0),
            "a durably parked checkpoint starts without an execution claim"
        );

        store.set_fail_release(true);
        let err = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect_err("a failed continuation claim release must reject the denial");
        assert!(
            err.to_string()
                .contains("failed to release exec claim after routing checkpoint denial"),
            "unexpected error: {err}"
        );
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "the rejected continuation rolls back to the pre-decision checkpoint"
        );
        assert!(
            !engine
                .claims_retained_after_terminal_rollback
                .contains(&run_id),
            "a CONTINUED run must not be tracked as a terminal-rollback retention (else it is heartbeated forever)"
        );

        // The stale claim lingers durably only until the reaper collects it: it is NOT
        // heartbeated (not retained, run is parked), so once its lease lapses a
        // maintenance tick frees the slot — no permanent double-pin.
        store.set_fail_release(false);
        store.expire_claim_now(&run_id);
        let _ = engine.run_maintenance_tick();
        assert_eq!(
            store.claim_counts("det-cp-deny-goto-release-fail").unwrap(),
            (0, 0),
            "the stale continuation claim is reaped, not renewed forever"
        );
        assert!(
            engine.can_start("det-cp-deny-goto-release-fail"),
            "the freed slot is available again after the stale claim is reaped"
        );
    }

    #[test]
    fn restore_reconciles_stale_terminal_rollback_marker_on_retried_checkpoint() {
        // Crash-window reconcile: a denied checkpoint whose failure route (Retry)
        // re-parks at the SAME checkpoint CONTINUES the run. If the continuation claim
        // release fails and the daemon then restarts before the lease reaper runs, the
        // durable terminal-rollback marker survives on a run that already recorded a
        // Failed result for its current step. `restore_runs` must recognise that marker
        // as stale (a completed continuation, not a genuine terminal rollback) and
        // RELEASE it rather than renew it forever.
        let store = std::sync::Arc::new(FailingSaveLeasedStore::healthy());
        let mut sop = deterministic_sop("det-cp-deny-retry-reconcile");
        sop.steps[1].on_failure = StepFailure::Retry { max: 2 };
        sop.max_concurrent = 1;

        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-retry-reconcile", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        let checkpoint = engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();
        assert!(matches!(checkpoint, SopRunAction::CheckpointWait { .. }));

        store.set_fail_release(true);
        let err = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect_err("a failed continuation claim release must reject the denial");
        assert!(
            err.to_string()
                .contains("failed to release exec claim after routing checkpoint denial"),
            "unexpected error: {err}"
        );
        // Precondition for the crash-window: the release failure left the durable marker
        // live on the (Retry-)continued run.
        assert!(
            store.has_retained_terminal_rollback_claim(&run_id).unwrap(),
            "the failed release leaves a stale durable terminal-rollback marker"
        );

        // Simulate a restart: the transient release fault has cleared.
        store.set_fail_release(false);
        let mut restored = engine_with_sops(vec![{
            let mut s = deterministic_sop("det-cp-deny-retry-reconcile");
            s.steps[1].on_failure = StepFailure::Retry { max: 2 };
            s.max_concurrent = 1;
            s
        }])
        .with_store(store.clone());
        restored.restore_runs();

        assert_eq!(
            restored.get_run(&run_id).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "restart restores the parked checkpoint run normally"
        );
        assert!(
            !store.has_retained_terminal_rollback_claim(&run_id).unwrap(),
            "restore must reconcile the stale marker away, not renew it"
        );
        assert!(
            !restored
                .claims_retained_after_terminal_rollback
                .contains(&run_id),
            "a reconciled run must not be tracked for terminal-rollback heartbeating"
        );
        assert_eq!(
            store.claim_counts("det-cp-deny-retry-reconcile").unwrap(),
            (0, 0),
            "the stale terminal-rollback claim is released on restore"
        );
        assert!(
            restored.can_start("det-cp-deny-retry-reconcile"),
            "a continued parked checkpoint must not keep the execution slot after restart"
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

    #[tokio::test]
    async fn sop_approve_tool_resumes_deterministic_checkpoint() {
        // Regression guard: the sop_approve tool must route a
        // PausedCheckpoint to approve_step, because resolve_gate reports NotWaiting
        // for it. Without that routing the tool answers "not waiting for approval"
        // and a deterministic run is stuck unresumable through every surface.
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
    fn deny_checkpoint_goto_continuation_respects_per_sop_cap() {
        // A denied checkpoint whose `on_failure = Goto` CONTINUES execution, so it must
        // pass the same capped store CAS as every other resume-to-continue path. With
        // max_concurrent = 1 and the slot already taken, denying a parked checkpoint
        // returns typed backpressure and leaves it parked - it does NOT resume above cap.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = deterministic_sop("det-cp");
        sop.max_concurrent = 1;
        sop.steps[1].on_failure = StepFailure::Goto { step: 3 };
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());

        let a = engine.start_run("det-cp", manual_event()).unwrap();
        let id_a = extract_run_id(&a).to_string();
        engine
            .advance_deterministic_step(&id_a, serde_json::json!("a1"), None)
            .unwrap();
        let b = engine.start_run("det-cp", manual_event()).unwrap();
        let id_b = extract_run_id(&b).to_string();
        engine
            .advance_deterministic_step(&id_b, serde_json::json!("b1"), None)
            .unwrap();
        assert_eq!(
            store.claim_counts("det-cp").unwrap(),
            (0, 0),
            "both parked at the checkpoint: no exec claim held"
        );

        // Approve A -> it takes the one slot.
        engine.approve_step(&id_a).unwrap();
        assert_eq!(
            store.claim_counts("det-cp").unwrap().0,
            1,
            "A holds the one slot"
        );

        // Deny B's checkpoint: its Goto continuation must be refused at capacity.
        let err = engine
            .decide_checkpoint(
                &id_b,
                ApprovalDecision::Deny {
                    reason: Some("nope".into()),
                },
            )
            .expect_err("a denied Goto continuation must be refused at capacity");
        assert!(
            err_is_resume_at_capacity(&err),
            "the refusal is typed capacity backpressure, not a fault: {err}"
        );
        assert_eq!(
            engine.get_run(&id_b).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "B stays paused at the checkpoint, re-resolvable"
        );
        assert_eq!(
            store.claim_counts("det-cp").unwrap().0,
            1,
            "still exactly one slot in use, not two"
        );
    }

    #[test]
    fn deny_checkpoint_retry_continuation_respects_global_cap() {
        // A denied checkpoint whose `on_failure = Retry` (budget remaining) CONTINUES,
        // so it is capped against the GLOBAL limit too. Two SOPs share
        // max_concurrent_total = 1; with the one global slot taken, denying a parked
        // checkpoint on the other returns typed backpressure and stays parked. A
        // terminal denial (Fail, or Retry exhausted) would instead stay uncapped.
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut s1 = deterministic_sop("det-a");
        s1.max_concurrent = 1;
        let mut s2 = deterministic_sop("det-b");
        s2.max_concurrent = 1;
        s2.steps[1].on_failure = StepFailure::Retry { max: 3 };
        let cfg = SopConfig {
            max_concurrent_total: 1,
            ..SopConfig::default()
        };
        let mut engine = engine_with_config_sops(cfg, vec![s1, s2]).with_store(store.clone());

        let a = engine.start_run("det-a", manual_event()).unwrap();
        let id_a = extract_run_id(&a).to_string();
        engine
            .advance_deterministic_step(&id_a, serde_json::json!("a1"), None)
            .unwrap();
        let b = engine.start_run("det-b", manual_event()).unwrap();
        let id_b = extract_run_id(&b).to_string();
        engine
            .advance_deterministic_step(&id_b, serde_json::json!("b1"), None)
            .unwrap();

        // Approve det-a -> it takes the one global slot.
        engine.approve_step(&id_a).unwrap();
        assert_eq!(
            store.claim_counts("det-a").unwrap().1,
            1,
            "the one global slot is taken"
        );

        // Deny det-b's checkpoint: its Retry continuation is refused at the global cap.
        let err = engine
            .decide_checkpoint(
                &id_b,
                ApprovalDecision::Deny {
                    reason: Some("nope".into()),
                },
            )
            .expect_err("a denied Retry continuation must be refused at the global cap");
        assert!(
            err_is_resume_at_capacity(&err),
            "the refusal is typed capacity backpressure: {err}"
        );
        assert_eq!(
            engine.get_run(&id_b).unwrap().status,
            SopRunStatus::PausedCheckpoint,
            "det-b stays paused, re-resolvable"
        );
        assert_eq!(
            store.claim_counts("det-b").unwrap().1,
            1,
            "still exactly one global slot in use, not two"
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
    fn deny_checkpoint_goto_rolls_back_when_active_save_fails() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
        });
        let mut sop = deterministic_sop("det-cp-deny-goto-save-fail");
        sop.steps[1].on_failure = StepFailure::Goto { step: 3 };
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-goto-save-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();

        let before = engine.get_run(&run_id).unwrap();
        let prior_waiting_since = before.waiting_since.clone();
        let prior_step_results = before.step_results.len();
        let prior_current_step = before.current_step;
        assert_eq!(
            store.claim_counts("det-cp-deny-goto-save-fail").unwrap(),
            (0, 0),
            "the checkpoint must be durably parked before the save failure is injected"
        );

        store
            .fail_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect_err("active save failure must reject the denied checkpoint transition");
        assert!(
            err.to_string()
                .contains("failed to persist checkpoint denial transition"),
            "unexpected error: {err}"
        );

        let restored = engine.get_run(&run_id).unwrap();
        assert_eq!(restored.status, SopRunStatus::PausedCheckpoint);
        assert_eq!(restored.current_step, prior_current_step);
        assert_eq!(restored.waiting_since, prior_waiting_since);
        assert_eq!(restored.step_results.len(), prior_step_results);
        assert_eq!(
            store.claim_counts("det-cp-deny-goto-save-fail").unwrap(),
            (0, 0),
            "the claim reacquired for the rejected denial must be released"
        );
        let events = store.list_events(&run_id).unwrap();
        assert!(
            !events.iter().any(|event| event.kind == "checkpoint_denied"),
            "a failed denied-checkpoint transition must not emit checkpoint_denied: {events:?}"
        );
    }

    #[test]
    fn deny_checkpoint_retry_rolls_back_when_active_save_fails() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
        });
        let mut sop = deterministic_sop("det-cp-deny-retry-save-fail");
        sop.steps[1].on_failure = StepFailure::Retry { max: 2 };
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-retry-save-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();

        let before = engine.get_run(&run_id).unwrap();
        let prior_waiting_since = before.waiting_since.clone();
        let prior_step_results = before.step_results.len();
        let prior_current_step = before.current_step;
        assert_eq!(
            store.claim_counts("det-cp-deny-retry-save-fail").unwrap(),
            (0, 0),
            "the checkpoint must be durably parked before the save failure is injected"
        );

        store
            .fail_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect_err("active save failure must reject the denied checkpoint retry");
        assert!(
            err.to_string()
                .contains("failed to persist checkpoint denial transition"),
            "unexpected error: {err}"
        );

        let restored = engine.get_run(&run_id).unwrap();
        assert_eq!(restored.status, SopRunStatus::PausedCheckpoint);
        assert_eq!(restored.current_step, prior_current_step);
        assert_eq!(restored.waiting_since, prior_waiting_since);
        assert_eq!(restored.step_results.len(), prior_step_results);
        assert_eq!(
            store.claim_counts("det-cp-deny-retry-save-fail").unwrap(),
            (0, 0),
            "the claim reacquired for the rejected retry denial must be released"
        );
        let events = store.list_events(&run_id).unwrap();
        assert!(
            !events.iter().any(|event| event.kind == "checkpoint_denied"),
            "a failed denied-checkpoint retry must not emit checkpoint_denied: {events:?}"
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
    fn deny_checkpoint_keeps_claim_when_terminal_persist_fails() {
        let store = std::sync::Arc::new(FailingAppendStore {
            inner: InMemoryRunStore::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fail_save: std::sync::atomic::AtomicBool::new(false),
            fail_finish: std::sync::atomic::AtomicBool::new(false),
        });
        let mut sop = deterministic_sop("det-cp-deny-finish-fail");
        sop.max_concurrent = 1;
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-finish-fail", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();

        let before = engine.get_run(&run_id).unwrap();
        let prior_waiting_since = before.waiting_since.clone();
        let prior_step_results = before.step_results.len();
        let prior_current_step = before.current_step;
        assert_eq!(
            store.claim_counts("det-cp-deny-finish-fail").unwrap(),
            (0, 0),
            "a durably parked checkpoint starts without an execution claim"
        );

        store
            .fail_finish
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect_err("terminal persistence failure must reject the decision");
        assert!(err.to_string().contains("injected finish failure"));

        let restored = engine.get_run(&run_id).unwrap();
        assert_eq!(restored.status, SopRunStatus::PausedCheckpoint);
        assert_eq!(restored.current_step, prior_current_step);
        assert_eq!(restored.waiting_since, prior_waiting_since);
        assert_eq!(restored.step_results.len(), prior_step_results);
        assert_eq!(
            store.claim_counts("det-cp-deny-finish-fail").unwrap(),
            (1, 1),
            "a failed terminal write keeps the reacquired claim fail-closed"
        );
    }

    #[test]
    fn deny_checkpoint_preflights_invalid_failure_goto_without_mutation() {
        let store = std::sync::Arc::new(InMemoryRunStore::new());
        let mut sop = deterministic_sop("det-cp-deny-invalid-goto");
        sop.steps[1].on_failure = StepFailure::Goto { step: 99 };
        let mut engine = engine_with_sops(vec![sop]).with_store(store.clone());
        let action = engine
            .start_run("det-cp-deny-invalid-goto", manual_event())
            .unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine
            .advance_deterministic_step(&run_id, serde_json::json!("s1-out"), None)
            .unwrap();

        let before = engine.get_run(&run_id).unwrap();
        let prior_waiting_since = before.waiting_since.clone();
        let prior_step_results = before.step_results.len();
        let prior_current_step = before.current_step;
        let err = engine
            .decide_checkpoint(&run_id, ApprovalDecision::Deny { reason: None })
            .expect_err("an invalid failure-route target must be rejected before mutation");
        assert!(err.to_string().contains("step 99"));

        let restored = engine.get_run(&run_id).unwrap();
        assert_eq!(restored.status, SopRunStatus::PausedCheckpoint);
        assert_eq!(restored.current_step, prior_current_step);
        assert_eq!(restored.waiting_since, prior_waiting_since);
        assert_eq!(restored.step_results.len(), prior_step_results);
        assert_eq!(
            store.claim_counts("det-cp-deny-invalid-goto").unwrap(),
            (0, 0),
            "preflight must not acquire a claim for an invalid failure route"
        );
        assert!(
            !store
                .list_events(&run_id)
                .unwrap()
                .iter()
                .any(|event| event.kind == "checkpoint_denied"),
            "an invalid route must not leave a denied-checkpoint event behind"
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
