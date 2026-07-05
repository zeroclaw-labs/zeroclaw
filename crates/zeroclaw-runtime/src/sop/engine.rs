use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};

use super::condition::evaluate_condition;
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
    DeterministicRunState, DeterministicSavings, FilesystemEventKind, Sop, SopEvent,
    SopExecutionMode, SopPriority, SopRun, SopRunAction, SopRunStatus, SopStep, SopStepKind,
    SopStepResult, SopStepStatus, SopTrigger, SopTriggerSource,
};
use crate::calendar::{CALENDAR_NO_SHOW_TOPIC, CalendarNoShowEvent};
use crate::security::{ContentSafety, new_marker_id};
use serde_json::Value;
use zeroclaw_config::schema::{ApprovalMode, SopConfig};

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

    /// Reconstruct in-flight runs from the store at startup (durable backends).
    /// No-op for the in-memory default. Does not overwrite already-present runs.
    pub fn restore_runs(&mut self) {
        match self.store.load_active_runs() {
            Ok(runs) => {
                let mut restored = 0usize;
                for pr in runs {
                    // Re-establish the claim WITHOUT admission caps: a restored run
                    // was already admitted before the restart, so reconstruction is
                    // not new admission. This keeps `active_runs` and the live-claim
                    // count aligned 1:1 even for an over-cap restored set (the old
                    // capped `try_claim_run` silently dropped the claim over cap,
                    // leaving a locally active run with no store claim). On a renew
                    // error the run is left out of `active_runs` rather than cached
                    // orphaned, and the failure is logged loudly.
                    if let Err(e) = self
                        .store
                        .renew_claim_for_restore(&pr.run.run_id, &pr.run.sop_name)
                    {
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
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"restored": restored})),
                        &format!("SOP engine restored {restored} run(s) from store")
                    );
                }
            }
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "SOP engine: failed to restore runs from store"
            ),
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
        if let Some(run) = self.active_runs.get(run_id) {
            self.heartbeat_claim_for_run(run);
            let mut pr = PersistedRun::new(run.clone(), now_iso8601(), run.trigger_event.source);
            // Each persist is a new state revision; the store rejects a
            // same-revision divergent write, so advance past what is stored.
            pr.revision = self.next_run_revision(run_id);
            if let Err(e) = self.store.save_run(&pr) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"run_id": run_id, "error": e.to_string()})
                        ),
                    "SOP engine: failed to persist run"
                );
            }
        }
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
        for run in self.active_runs.values() {
            self.heartbeat_claim_for_run(run);
        }
    }

    /// Persist a run that has reached a terminal state (best-effort).
    fn persist_terminal(&self, run: &SopRun) {
        let mut pr = PersistedRun::new(run.clone(), now_iso8601(), run.trigger_event.source);
        // The terminal write is the run's final revision; advance past the last
        // active snapshot so the store's revision guard accepts it.
        pr.revision = self.next_run_revision(&run.run_id);
        if let Err(e) = self.store.finish_run(&run.run_id, &pr) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"run_id": run.run_id, "error": e.to_string()})
                    ),
                "SOP engine: failed to persist terminal run"
            );
        }
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

    // ── Run lifecycle ───────────────────────────────────────────

    /// Check whether a new run can be started for the given SOP
    /// (respects cooldown and concurrency limits).
    pub fn can_start(&self, sop_name: &str) -> bool {
        let sop = match self.get_sop(sop_name) {
            Some(s) => s,
            None => return false,
        };

        // Concurrency limits are backed by the store's live CAS claims so
        // multiple engine holders observe the same admission source.
        let (active_for_sop, active_total) = match self.store.claim_counts(sop_name) {
            Ok(counts) => counts,
            Err(_) => (
                self.active_runs
                    .values()
                    .filter(|r| r.sop_name == sop_name)
                    .count(),
                self.active_runs.len(),
            ),
        };
        if active_for_sop >= sop.max_concurrent as usize
            || active_total >= self.config.max_concurrent_total
        {
            return false;
        }

        // Cooldown: the last terminal completion is read from the shared store so
        // every engine holder observes the same marker (an engine that did not run
        // the SOP has no local finished entry). Fall back to the in-memory
        // `last_finished_run` only if the store call errors - mirrors the
        // claim-counts store->memory fallback above.
        if sop.cooldown_secs > 0 {
            let last_completed = match self.store.last_terminal_completed_at(sop_name) {
                Ok(completed) => completed,
                Err(_) => self
                    .last_finished_run(sop_name)
                    .and_then(|last| last.completed_at.clone()),
            };
            if let Some(completed_at) = last_completed
                && !cooldown_elapsed(&completed_at, sop.cooldown_secs)
            {
                return false;
            }
        }

        true
    }

    /// Start a new SOP run. Returns the first action to take.
    /// Deterministic SOPs are automatically routed to `start_deterministic_run`.
    pub fn start_run(&mut self, sop_name: &str, event: SopEvent) -> Result<SopRunAction> {
        // Route deterministic SOPs to dedicated path
        if self
            .get_sop(sop_name)
            .is_some_and(|s| s.execution_mode == SopExecutionMode::Deterministic)
        {
            return self.start_deterministic_run(sop_name, event);
        }

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
        let action = resolve_step_action(
            sop,
            &step,
            run_id.to_string(),
            context,
            self.config.approval_mode,
        );
        if matches!(action, SopRunAction::WaitApproval { .. })
            && let Some(run) = self.active_runs.get_mut(run_id)
        {
            run.status = SopRunStatus::WaitingApproval;
            run.waiting_since = Some(now_iso8601());
        }

        self.persist_active(run_id);
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
        self.finish_run(run_id, SopRunStatus::Cancelled, None);
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
        run.status = SopRunStatus::Running;
        run.waiting_since = None;
        self.advance_deterministic_step(run_id, piped, None)
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
    /// on an out-of-range step index (which would poison the engine mutex).
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

        let step_idx = (current_step - 1) as usize;
        let step = sop.steps.get(step_idx).cloned().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"sop_name": sop_name, "step": current_step})),
                "SOP engine: step no longer exists (definition changed mid-run)"
            );
            anyhow::Error::msg(format!(
                "SOP '{sop_name}' step {current_step} no longer exists (definition changed mid-run)"
            ))
        })?;

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

        // The lookups succeeded; commit the transition.
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::Error::msg(format!("Active run not found: {run_id}")))?;
        run.status = SopRunStatus::Running;
        run.waiting_since = None;
        let context = format_step_context(&sop, run, &step, &self.config);

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
        };

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

        match self.dispatch_deterministic_step(&run_id, &sop, 1, serde_json::Value::Null) {
            Ok(action) => Ok(action),
            Err(e) => {
                self.active_runs.remove(&run_id);
                self.release_claim_best_effort(&claim);
                Err(e)
            }
        }
    }

    /// Drive a just-started headless deterministic run to a terminal state.
    ///
    /// Channel-sourced dispatch (filesystem, MQTT, peripheral, cron) has no
    /// agent loop to execute steps, so a deterministic run that returns
    /// `DeterministicStep` would otherwise sit in `active_runs` as `Running`
    /// forever, consuming its `max_concurrent` slot and blocking every later
    /// event from the same SOP. Advancing each step here drains the chain to
    /// `Completed`, which evicts the run via `finish_run` and frees the slot so
    /// the next matching event can fire. A `CheckpointWait` is intentionally
    /// left paused (an operator gate, not a stuck run).
    pub fn drive_headless_deterministic(
        &mut self,
        run_id: &str,
        first_action: SopRunAction,
    ) -> Result<SopRunAction> {
        let mut action = first_action;
        loop {
            match action {
                SopRunAction::DeterministicStep { ref input, .. } => {
                    let piped = input.clone();
                    action = self.advance_deterministic_step(run_id, piped, None)?;
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

        // Record step result
        let (started_at, completed_at) = match step_timestamps {
            Some((started, completed)) => (started, completed),
            None => (run.started_at.clone(), Some(now_iso8601())),
        };
        let step_result = SopStepResult {
            step_number: run.current_step,
            status: SopStepStatus::Completed,
            output: step_output.to_string(),
            started_at,
            completed_at,
        };
        let retry_input = retry_input_value(run, current_step.number);
        run.step_results.push(step_result);

        let mut last_status = SopStepStatus::Completed;
        if let Err(reason) = self.validate_step_output(&current_step, &step_output) {
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
            // Each deterministic step saves one LLM call only when the step
            // produced a valid completed output.
            run.llm_calls_saved += 1;
        }

        self.route_recorded_step(
            run_id,
            &sop,
            &current_step,
            last_status,
            true,
            Some(retry_input),
            Some(step_output),
        )
    }

    /// Resume a deterministic run from persisted state.
    pub fn resume_deterministic_run(
        &mut self,
        state: DeterministicRunState,
    ) -> Result<SopRunAction> {
        let run = self.active_runs.get_mut(&state.run_id).ok_or_else(|| {
            let run_id = state.run_id.clone();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"run_id": run_id})),
                "SOP engine: active run not found"
            );
            anyhow::Error::msg(format!("Active run not found: {}", state.run_id))
        })?;

        if run.status != SopRunStatus::PausedCheckpoint {
            bail!(
                "Run {} is not paused at checkpoint (status: {})",
                state.run_id,
                run.status
            );
        }

        let sop = self
            .sops
            .iter()
            .find(|s| s.name == run.sop_name)
            .ok_or_else(|| {
                let sop_name = run.sop_name.clone();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"sop_name": sop_name})),
                    "SOP engine: sop no longer loaded (definition removed mid-run)"
                );
                anyhow::Error::msg(format!("SOP '{}' no longer loaded", run.sop_name))
            })?
            .clone();

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
                });
            }
        }

        let last_output = state
            .step_outputs
            .get(&state.last_completed_step)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let run_id = state.run_id.clone();

        if state.last_completed_step == 0 {
            return self.dispatch_deterministic_step(&run_id, &sop, 1, last_output);
        }

        {
            let run = self.active_runs.get_mut(&run_id).unwrap();
            run.current_step = state.last_completed_step;
        }
        let current_step = self.resolve_sop_step(&sop, state.last_completed_step)?;
        self.route_recorded_step(
            &run_id,
            &sop,
            &current_step,
            SopStepStatus::Completed,
            true,
            None,
            Some(last_output),
        )
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

        if step.kind == SopStepKind::Checkpoint {
            // Pause at checkpoint — persist state and wait for approval
            if let Some(run) = self.active_runs.get_mut(run_id) {
                run.status = SopRunStatus::PausedCheckpoint;
                run.waiting_since = Some(now_iso8601());
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
            // row for restore_runs() to rehydrate.
            self.persist_active(run_id);

            Ok(SopRunAction::CheckpointWait {
                run_id: run_id.to_string(),
                step: step.clone(),
                state_file,
            })
        } else {
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
        let mut run = self.active_runs.remove(run_id).unwrap();
        run.status = status;
        run.completed_at = Some(now_iso8601());
        let sop_name = run.sop_name.clone();
        let run_id_owned = run.run_id.clone();
        self.metrics.record_run_complete(&run);
        self.persist_terminal(&run);
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
fn trigger_matches(trigger: &SopTrigger, event: &SopEvent) -> bool {
    match (trigger, event.source) {
        (SopTrigger::Mqtt { topic, condition }, SopTriggerSource::Mqtt) => {
            let topic_match = event
                .topic
                .as_deref()
                .is_some_and(|t| mqtt_topic_matches(topic, t));
            if !topic_match {
                return false;
            }
            // Evaluate condition against payload (None condition = unconditional)
            match condition {
                Some(cond) => evaluate_condition(cond, event.payload.as_deref()),
                None => true,
            }
        }

        (
            SopTrigger::Amqp {
                routing_key,
                condition,
            },
            SopTriggerSource::Amqp,
        ) => {
            let key_match = event
                .topic
                .as_deref()
                .is_some_and(|t| amqp_routing_key_matches(routing_key, t));
            if !key_match {
                return false;
            }
            match condition {
                Some(cond) => evaluate_condition(cond, event.payload.as_deref()),
                None => true,
            }
        }

        (SopTrigger::Webhook { path }, SopTriggerSource::Webhook) => {
            event.topic.as_deref().is_some_and(|t| t == path)
        }

        (
            SopTrigger::Peripheral {
                board,
                signal,
                condition,
            },
            SopTriggerSource::Peripheral,
        ) => {
            let topic_match = event.topic.as_deref().is_some_and(|t| {
                let expected = format!("{board}/{signal}");
                t == expected
            });
            if !topic_match {
                return false;
            }
            // Evaluate condition against payload (None condition = unconditional)
            match condition {
                Some(cond) => evaluate_condition(cond, event.payload.as_deref()),
                None => true,
            }
        }

        (SopTrigger::Cron { expression }, SopTriggerSource::Cron) => {
            event.topic.as_deref().is_some_and(|t| t == expression)
        }

        (
            SopTrigger::Filesystem {
                path,
                events,
                condition,
            },
            SopTriggerSource::Filesystem,
        ) => {
            let path_match = event
                .topic
                .as_deref()
                .is_some_and(|t| filesystem_path_matches(path, t));
            if !path_match {
                return false;
            }
            if !events.is_empty() && !filesystem_event_listed(events, event.payload.as_deref()) {
                return false;
            }
            match condition {
                Some(cond) => evaluate_condition(cond, event.payload.as_deref()),
                None => true,
            }
        }

        (
            SopTrigger::Calendar {
                calendar_source,
                calendar_ids,
            },
            SopTriggerSource::Calendar,
        ) => calendar_trigger_matches(calendar_source, calendar_ids, event),

        (SopTrigger::Manual, SopTriggerSource::Manual) => true,

        _ => false,
    }
}

fn calendar_trigger_matches(
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
fn mqtt_topic_matches(pattern: &str, topic: &str) -> bool {
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
fn amqp_routing_key_matches(pattern: &str, key: &str) -> bool {
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
fn filesystem_path_matches(pattern: &str, path: &str) -> bool {
    if let Ok(compiled) = glob::Pattern::new(pattern)
        && compiled.matches(path)
    {
        return true;
    }
    let prefix = pattern.trim_end_matches('/');
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Whether the payload's `event` field names one of the trigger's listed kinds.
fn filesystem_event_listed(events: &[FilesystemEventKind], payload: Option<&str>) -> bool {
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
fn resolve_step_action(
    sop: &Sop,
    step: &SopStep,
    run_id: String,
    context: String,
    approval_mode: ApprovalMode,
) -> SopRunAction {
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
    let mut needs_approval = execution_mode_needs_approval(effective_mode, sop, step);
    if approval_mode == ApprovalMode::OutOfBandRequired && sop_needs_approval && !needs_approval {
        needs_approval = true;
    }

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
    fn step_mode_can_relax_step_by_step_step() {
        let mut sop = test_sop("s1", SopExecutionMode::StepByStep, SopPriority::Normal);
        sop.steps[0].mode = Some(SopExecutionMode::Auto);
        let mut engine = engine_with_sops(vec![sop]);

        let action = engine.start_run("s1", manual_event()).unwrap();

        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));
    }

    #[test]
    fn out_of_band_required_prevents_step_auto_relaxing_gate() {
        let mut sop = test_sop("s1", SopExecutionMode::StepByStep, SopPriority::Normal);
        sop.steps[0].mode = Some(SopExecutionMode::Auto);
        let mut engine = engine_with_config_sops(
            SopConfig {
                approval_mode: ApprovalMode::OutOfBandRequired,
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
                },
            )
            .unwrap();
        assert!(
            matches!(action, SopRunAction::Completed { .. }),
            "deterministic run should complete after the post-checkpoint step"
        );
    }

    #[tokio::test]
    async fn sop_approve_tool_resumes_deterministic_checkpoint() {
        // Regression guard (#8304 review): the sop_approve tool must route a
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
        engine.persist_terminal(&run);
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
}
