use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use crate::sop::types::{SopRunAction, SopStepResult, SopStepStatus};
use crate::sop::{SopAuditLogger, SopEngine, SopMetricsCollector};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

/// Report a step result and advance an SOP run to the next step.
pub struct SopAdvanceTool {
    engine: Arc<Mutex<SopEngine>>,
    audit: Option<Arc<SopAuditLogger>>,
    collector: Option<Arc<SopMetricsCollector>>,
    /// Sealed ceiling of a bounded delegate target this instance was rebuilt
    /// for, `None` for the normal (unbounded) registration. See
    /// `SopExecuteTool::caller_ceiling` — same reasoning, same shared engine.
    caller_ceiling: Option<crate::tools::caller_ceiling::CallerCeiling>,
}

impl SopAdvanceTool {
    pub fn new(engine: Arc<Mutex<SopEngine>>) -> Self {
        Self {
            engine,
            audit: None,
            collector: None,
            caller_ceiling: None,
        }
    }

    pub fn with_audit(mut self, audit: Arc<SopAuditLogger>) -> Self {
        self.audit = Some(audit);
        self
    }

    pub fn with_collector(mut self, collector: Arc<SopMetricsCollector>) -> Self {
        self.collector = Some(collector);
        self
    }

    pub fn with_caller_ceiling(
        mut self,
        ceiling: Option<crate::tools::caller_ceiling::CallerCeiling>,
    ) -> Self {
        self.caller_ceiling = ceiling;
        self
    }

    /// A fresh instance sharing this one's live `engine`/`audit`/`collector`
    /// handles, bound to `ceiling`. See `SopExecuteTool::rebound_with_ceiling`.
    pub(crate) fn rebound_with_ceiling(
        &self,
        ceiling: crate::tools::caller_ceiling::CallerCeiling,
    ) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            audit: self.audit.clone(),
            collector: self.collector.clone(),
            caller_ceiling: Some(ceiling),
        }
    }
}

#[async_trait]
impl Tool for SopAdvanceTool {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn name(&self) -> &str {
        "sop_advance"
    }

    fn description(&self) -> &str {
        "Report the result of the current SOP step and advance to the next step. Provide the run_id, whether the step succeeded or failed, and a brief output summary."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "The run ID to advance"
                },
                "status": {
                    "type": "string",
                    "enum": ["completed", "failed", "skipped"],
                    "description": "Result status of the current step"
                },
                "output": {
                    "type": "string",
                    "description": "Brief summary of what happened in this step"
                }
            },
            "required": ["run_id", "status", "output"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let run_id = args.get("run_id").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "run_id"})),
                "tool argument validation failed"
            );

            anyhow::Error::msg("Missing 'run_id' parameter")
        })?;

        let status_str = args.get("status").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "status"})),
                "tool argument validation failed"
            );

            anyhow::Error::msg("Missing 'status' parameter")
        })?;

        let output = args.get("output").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "output"})),
                "tool argument validation failed"
            );

            anyhow::Error::msg("Missing 'output' parameter")
        })?;

        let step_status = match status_str {
            "completed" => SopStepStatus::Completed,
            "failed" => SopStepStatus::Failed,
            "skipped" => SopStepStatus::Skipped,
            other => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Invalid status '{other}'. Must be: completed, failed, or skipped"
                    )),
                });
            }
        };

        // Lock engine, advance step, snapshot data for audit, then drop lock
        let (action, step_result_ok, finished_run) = {
            let mut engine = self.engine.lock().map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "SOP engine lock poisoned"
                );

                anyhow::Error::msg(format!("Engine lock poisoned: {e}"))
            })?;

            let current_step = engine
                .get_run(run_id)
                .map(|r| r.current_step)
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"run_id": run_id})),
                        "sop_advance tool: run not found"
                    );
                    anyhow::Error::msg(format!("Run not found: {run_id}"))
                })?;

            let now = now_iso8601();
            let step_result = SopStepResult {
                step_number: current_step,
                status: step_status,
                output: output.to_string(),
                started_at: now.clone(),
                completed_at: Some(now),
                // Agent-reported advance of the current inline step; the tool
                // has no resolved alias context of its own.
                effective_agent: None,
                tool_calls: Vec::new(),
            };
            let step_result_clone = step_result.clone();

            match engine.advance_step(run_id, step_result) {
                Ok(action) => {
                    // Same fail-closed rule as `SopExecuteTool::execute`: a
                    // bounded caller must never leave a run parked, whether it
                    // started that run or is merely advancing one someone
                    // else started. Checked and cancelled under the SAME
                    // engine lock `advance_step` used.
                    if self.caller_ceiling.is_some()
                        && let Some(parked_id) = crate::sop::executor::parked_run_id(&action)
                    {
                        let parked_id = parked_id.to_string();
                        // Same reporting rule as `SopExecuteTool::execute`:
                        // `cancel_run` persists the terminal record BEFORE
                        // removing the run from `active_runs`, so a
                        // persistence failure leaves it genuinely still
                        // active - report that failure, not a false success.
                        if let Err(e) = engine.cancel_run(&parked_id) {
                            ::zeroclaw_log::record!(
                                ERROR,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Fail
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "run_id": parked_id,
                                    "error": e.to_string(),
                                })),
                                "bounded sop_advance: parked run could not be cancelled"
                            );
                            return Ok(ToolResult {
                                success: false,
                                output: ToolOutput::default(),
                                error: Some(format!(
                                    "sop_advance: refused — run {parked_id} left outside this \
                                     turn (parked or pending on a dependency), which a bounded \
                                     caller's tool ceiling cannot follow past this turn; the run \
                                     could NOT be cancelled ({e}) and may still be active - \
                                     treat it as unresolved, not closed"
                                )),
                            });
                        }
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(format!(
                                "sop_advance: refused — run {parked_id} left outside this turn \
                                 (parked or pending on a dependency), which a bounded caller's \
                                 tool ceiling cannot follow past this turn; the run has been \
                                 cancelled"
                            )),
                        });
                    }
                    // Snapshot finished run for audit (Completed/Failed/Cancelled)
                    let finished = match &action {
                        SopRunAction::Completed { run_id, .. }
                        | SopRunAction::Cancelled { run_id, .. }
                        | SopRunAction::Failed { run_id, .. } => engine.get_run(run_id).cloned(),
                        _ => None,
                    };
                    // Only audit step result when advance succeeded
                    (Ok(action), Some(step_result_clone), finished)
                }
                Err(e) => (Err(e), None, None),
            }
        };

        // Audit logging (engine lock dropped, safe to await)
        if let Some(ref audit) = self.audit {
            if let Some(ref sr) = step_result_ok
                && let Err(e) = audit.log_step_result(run_id, sr).await
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "SOP audit log_step_result failed"
                );
            }
            if let Some(ref run) = finished_run
                && let Err(e) = audit.log_run_complete(run).await
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "SOP audit log_run_complete failed"
                );
            }
        }

        // Metrics collector (independent of audit)
        if let Some(ref collector) = self.collector
            && let Some(ref run) = finished_run
        {
            collector.record_run_complete(run);
        }

        if let Ok(ref action) = action {
            crate::sop::executor::enqueue_live_action(
                Arc::clone(&self.engine),
                self.audit.clone(),
                action,
                self.caller_ceiling.is_some(),
            );
        }

        match action {
            Ok(action) => {
                let result_output = match action {
                    SopRunAction::ExecuteStep {
                        run_id, context, ..
                    } => {
                        format!("Step recorded. Next step for run {run_id}:\n\n{context}")
                    }
                    SopRunAction::WaitApproval {
                        run_id, context, ..
                    } => {
                        format!(
                            "Step recorded. Next step for run {run_id} (waiting for approval):\n\n{context}"
                        )
                    }
                    SopRunAction::Completed { run_id, sop_name } => {
                        format!("SOP '{sop_name}' run {run_id} completed successfully.")
                    }
                    SopRunAction::Cancelled { run_id, sop_name } => {
                        format!("SOP '{sop_name}' run {run_id} was cancelled.")
                    }
                    SopRunAction::Failed {
                        run_id,
                        sop_name,
                        reason,
                    } => {
                        format!("SOP '{sop_name}' run {run_id} failed: {reason}")
                    }
                    SopRunAction::DeterministicStep { run_id, step, .. } => {
                        format!(
                            "Step recorded. Next deterministic step for run {run_id}: {}",
                            step.title
                        )
                    }
                    SopRunAction::CheckpointWait { run_id, step, .. } => {
                        format!(
                            "Step recorded. Run {run_id} paused at checkpoint: {}",
                            step.title
                        )
                    }
                    SopRunAction::Pending {
                        run_id,
                        step,
                        reason,
                        ..
                    } => {
                        format!("Step recorded. Run {run_id} pending before step {step}: {reason}")
                    }
                };
                Ok(ToolResult {
                    success: true,
                    output: result_output.into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to advance step: {e}")),
            }),
        }
    }
}

use crate::sop::engine::now_iso8601;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::engine::SopEngine;
    use crate::sop::types::*;
    use zeroclaw_config::schema::SopConfig;
    use zeroclaw_memory::Memory;

    fn test_sop() -> Sop {
        Sop {
            name: "test-sop".into(),
            description: "Test SOP".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Step one".into(),
                    body: "Do step one".into(),
                    suggested_tools: vec![],
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
            decision: None,
        }
    }

    fn engine_with_active_run() -> (Arc<Mutex<SopEngine>>, String) {
        engine_with_active_run_from(test_sop())
    }

    /// Same as `test_sop`, except step two requires confirmation — independent
    /// of `execution_mode` per its own doc comment — so advancing PAST step one
    /// parks on `WaitApproval` without needing a whole-run approval mode. Auto
    /// mode keeps step one's own start un-gated, isolating the park to the
    /// transition `sop_advance` is about to make.
    fn test_sop_gated_at_step_two() -> Sop {
        let mut sop = test_sop();
        sop.steps[1].requires_confirmation = true;
        sop
    }

    /// Step two declares a dependency on a step number that does not exist, so
    /// `route::eligible` (`sop/route/mod.rs:141-146`) is permanently false and
    /// advancing into it returns `Pending`, not `ExecuteStep` — the least
    /// obvious of the three variants `parked_run_id` must catch, since its own
    /// reason names a dependency, not an approval gate.
    fn test_sop_with_unmet_dependency_at_step_two() -> Sop {
        let mut sop = test_sop();
        sop.steps[1].routing.depends_on = vec![99];
        sop
    }

    fn engine_with_active_run_from(sop: Sop) -> (Arc<Mutex<SopEngine>>, String) {
        let mut engine = SopEngine::new(SopConfig::default());
        let name = sop.name.clone();
        engine.set_sops_for_test(vec![sop]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-02-19T12:00:00Z".into(),
        };
        engine.start_run(&name, event).unwrap();
        let run_id = engine
            .active_runs()
            .keys()
            .next()
            .expect("expected active run")
            .clone();
        (Arc::new(Mutex::new(engine)), run_id)
    }

    // ── bounded ceiling vs. a run that parks mid-flight ─────────────────────

    fn sealed_ceiling(names: &[&str]) -> crate::tools::caller_ceiling::CallerCeiling {
        let handle: crate::tools::caller_ceiling::CallerCeiling =
            Arc::new(std::sync::OnceLock::new());
        let _ = handle.set(names.iter().map(|n| (*n).to_string()).collect());
        handle
    }

    /// The other half of the bot's finding: not just STARTING a gated SOP, but
    /// ADVANCING one — whoever started it, `sop_advance` itself must not be the
    /// step that leaves a run parked and resolvable outside a bounded caller's
    /// ceiling. Positive control: `advance_to_next_step` above runs the exact
    /// same first transition with no ceiling and succeeds normally.
    #[tokio::test]
    async fn advance_into_a_park_under_ceiling_is_refused_and_the_run_cannot_be_approved() {
        let (engine, run_id) = engine_with_active_run_from(test_sop_gated_at_step_two());
        let tool = SopAdvanceTool::new(Arc::clone(&engine))
            .with_caller_ceiling(Some(sealed_ceiling(&["sop_advance"])));

        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "Step 1 done"
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a bounded caller must not be able to advance a run into a park: {result:?}"
        );
        let error = result.error.clone().unwrap_or_default();
        assert!(
            error.contains("refused") && error.contains("cancelled"),
            "refusal must name the cancellation, got: {error}"
        );

        let mut engine = engine.lock().unwrap();
        assert!(!engine.active_runs().contains_key(&run_id));
        assert_eq!(
            engine.get_run(&run_id).map(|r| r.status),
            Some(SopRunStatus::Cancelled)
        );
        let outcome = engine
            .resolve_gate(
                &run_id,
                crate::sop::approval::ApprovalDecision::Approve,
                crate::sop::approval::ApprovalPrincipal::cli(None),
            )
            .expect("resolving a cancelled run's gate must not error");
        // See the identical correction in sop_execute.rs's version of this
        // test: a terminal (Cancelled) run maps to `GateState::AlreadyResolved`
        // in `SopEngine::gate_state`, not `NotApplicable`/`NotWaiting`.
        assert!(
            !matches!(outcome, crate::sop::approval::ResolveOutcome::Resumed(_)),
            "an external approver must not be able to resume the cancelled run into a \
             fresh step, got: {outcome:?}"
        );
    }

    /// Same regression as `sop_execute.rs`'s equivalent test: `cancel_run`'s
    /// `Err` branch only logged the failure - the return below it still
    /// claimed the run was cancelled regardless. `start_run` only exercises
    /// `save_run`/`save_run_with_event` (not overridden here), so the run
    /// starts and reaches step two normally; only the CANCELLATION triggered
    /// by advancing into the park fails, via `finish_run_with_event`. The
    /// test above this one is this test's own positive control: same
    /// scenario, a store that persists, and it already asserts the message
    /// DOES say "cancelled".
    #[tokio::test]
    async fn advance_into_a_park_under_ceiling_reports_persistence_failure_instead_of_false_success()
     {
        let sop = test_sop_gated_at_step_two();
        let mut engine = SopEngine::new(SopConfig::default()).with_store(Arc::new(
            crate::sop::test_support::AlwaysFailFinishStore::new(),
        ));
        let name = sop.name.clone();
        engine.set_sops_for_test(vec![sop]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-02-19T12:00:00Z".into(),
        };
        engine.start_run(&name, event).unwrap();
        let run_id = engine
            .active_runs()
            .keys()
            .next()
            .expect("expected active run")
            .clone();
        let engine = Arc::new(Mutex::new(engine));
        let tool = SopAdvanceTool::new(Arc::clone(&engine))
            .with_caller_ceiling(Some(sealed_ceiling(&["sop_advance"])));

        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "Step 1 done"
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a bounded caller must still be refused even when cancellation itself fails: \
             {result:?}"
        );
        let error = result.error.clone().unwrap_or_default();
        assert!(
            !error.contains("has been cancelled"),
            "regression: the refusal must not claim the run was cancelled when the engine's \
             own cancel_run call failed (persistence error) - got: {error}"
        );

        let engine = engine.lock().unwrap();
        assert!(
            engine.active_runs().contains_key(&run_id),
            "the run must still be active: cancellation never persisted"
        );
    }

    /// The path this fix must NOT touch: a transition that does not park stays
    /// inside the current turn either way, and a bounded caller must still be
    /// able to make it — this is `advance_to_next_step` with a ceiling present.
    #[tokio::test]
    async fn advance_to_next_step_under_ceiling_still_succeeds() {
        let (engine, run_id) = engine_with_active_run();
        let tool =
            SopAdvanceTool::new(engine).with_caller_ceiling(Some(sealed_ceiling(&["sop_advance"])));

        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "Step 1 done successfully"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "a transition that does not park must not be refused under a ceiling: {result:?}"
        );
        assert!(result.output.contains("Step two"));
    }

    /// The third variant, for `sop_advance`: advancing into a step with an
    /// unmet dependency returns `Pending`, not a park variant, yet
    /// `run_maintenance_tick` can still later promote it to a real park with
    /// no caller in sight (see the doc comment on `executor::parked_run_id`).
    #[tokio::test]
    async fn advance_into_an_unmet_dependency_under_ceiling_is_refused_and_the_run_cannot_be_approved()
     {
        let (engine, run_id) =
            engine_with_active_run_from(test_sop_with_unmet_dependency_at_step_two());
        let tool = SopAdvanceTool::new(Arc::clone(&engine))
            .with_caller_ceiling(Some(sealed_ceiling(&["sop_advance"])));

        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "Step 1 done"
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a bounded caller must not be able to advance a run into a dependency-pending \
             state: {result:?}"
        );
        let error = result.error.clone().unwrap_or_default();
        assert!(
            error.contains("refused") && error.contains("cancelled"),
            "refusal must name the cancellation, got: {error}"
        );

        let engine = engine.lock().unwrap();
        assert!(!engine.active_runs().contains_key(&run_id));
        assert_eq!(
            engine.get_run(&run_id).map(|r| r.status),
            Some(SopRunStatus::Cancelled)
        );
    }

    #[tokio::test]
    async fn advance_to_next_step() {
        let (engine, run_id) = engine_with_active_run();
        let tool = SopAdvanceTool::new(engine);
        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "Step 1 done successfully"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Next step"));
        assert!(result.output.contains("Step two"));
    }

    #[tokio::test]
    async fn advance_to_completion() {
        let (engine, run_id) = engine_with_active_run();
        let tool = SopAdvanceTool::new(engine.clone());

        // Complete step 1
        tool.execute(json!({
            "run_id": run_id,
            "status": "completed",
            "output": "Step 1 done"
        }))
        .await
        .unwrap();

        // Complete step 2
        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "Step 2 done"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("completed successfully"));
    }

    #[tokio::test]
    async fn advance_with_failure() {
        let (engine, run_id) = engine_with_active_run();
        let tool = SopAdvanceTool::new(engine);
        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "failed",
                "output": "Valve stuck open"
            }))
            .await
            .unwrap();
        assert!(result.success); // tool succeeded, SOP failed
        assert!(result.output.contains("failed"));
        assert!(result.output.contains("Valve stuck open"));
    }

    #[tokio::test]
    async fn advance_invalid_status() {
        let (engine, run_id) = engine_with_active_run();
        let tool = SopAdvanceTool::new(engine);
        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "invalid",
                "output": "whatever"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid status"));
    }

    #[tokio::test]
    async fn advance_unknown_run() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tool = SopAdvanceTool::new(engine);
        let result = tool
            .execute(json!({
                "run_id": "nonexistent",
                "status": "completed",
                "output": "done"
            }))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn name_and_schema() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tool = SopAdvanceTool::new(engine);
        assert_eq!(tool.name(), "sop_advance");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["run_id"].is_object());
        assert!(schema["properties"]["status"]["enum"].is_array());
    }

    #[tokio::test]
    async fn advance_error_does_not_write_step_audit() {
        // Use a run_id that doesn't exist — advance_step will fail
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tmp = tempfile::tempdir().unwrap();
        let mem_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "sqlite".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let memory: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let audit = Arc::new(SopAuditLogger::new(memory.clone()));

        let tool = SopAdvanceTool::new(engine).with_audit(audit.clone());
        let result = tool
            .execute(json!({
                "run_id": "nonexistent",
                "status": "completed",
                "output": "done"
            }))
            .await;
        // advance_step on nonexistent run returns Err (anyhow)
        assert!(result.is_err());

        // Verify no phantom audit entries were written
        let runs = audit.list_runs().await.unwrap();
        assert!(
            runs.is_empty(),
            "no audit entries should exist after advance error"
        );
    }

    #[tokio::test]
    async fn advance_success_writes_step_audit() {
        let (engine, run_id) = engine_with_active_run();
        let tmp = tempfile::tempdir().unwrap();
        let mem_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "sqlite".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let memory: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let audit = Arc::new(SopAuditLogger::new(memory.clone()));

        let tool = SopAdvanceTool::new(engine).with_audit(audit.clone());
        let result = tool
            .execute(json!({
                "run_id": run_id,
                "status": "completed",
                "output": "Step 1 done"
            }))
            .await
            .unwrap();
        assert!(result.success);

        // Verify step audit was written
        let entries = memory
            .list(
                Some(&zeroclaw_memory::traits::MemoryCategory::Custom(
                    "sop".into(),
                )),
                None,
            )
            .await
            .unwrap();
        let step_keys: Vec<_> = entries
            .iter()
            .filter(|e| e.key.starts_with("sop_step_"))
            .collect();
        assert!(
            !step_keys.is_empty(),
            "step audit should be written on success"
        );
    }
}
