use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use crate::sop::types::{SopEvent, SopRunAction, SopTriggerSource};
use crate::sop::{SopAuditLogger, SopEngine};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

/// Manually trigger an SOP by name. Returns the run ID and first step instruction.
pub struct SopExecuteTool {
    engine: Arc<Mutex<SopEngine>>,
    audit: Option<Arc<SopAuditLogger>>,
    /// Sealed ceiling of a bounded delegate target this instance was rebuilt
    /// for, `None` for the normal (unbounded) registration. `sop_execute` is
    /// listed in `SAFE_FOR_BOUNDED_REUSE` — the caller's own live `engine`/
    /// `audit` handles are shared with the target rather than rebuilt — so
    /// this is the one piece that IS per-instance: see `rebound_with_ceiling`.
    caller_ceiling: Option<crate::tools::caller_ceiling::CallerCeiling>,
    /// The agent this tool instance belongs to. Recorded on runs it starts, so a
    /// procedure that parks at an approval can still resume as the agent that
    /// started it once the turn is gone.
    initiator: Option<String>,
}

impl SopExecuteTool {
    pub fn new(engine: Arc<Mutex<SopEngine>>) -> Self {
        Self {
            engine,
            audit: None,
            caller_ceiling: None,
            initiator: None,
        }
    }

    /// Record `alias` as the initiating agent on runs this tool starts.
    #[must_use]
    pub fn with_initiator(mut self, alias: impl Into<String>) -> Self {
        let alias = alias.into();
        self.initiator = (!alias.trim().is_empty()).then_some(alias);
        self
    }

    pub fn with_audit(mut self, audit: Arc<SopAuditLogger>) -> Self {
        self.audit = Some(audit);
        self
    }

    pub fn with_caller_ceiling(
        mut self,
        ceiling: Option<crate::tools::caller_ceiling::CallerCeiling>,
    ) -> Self {
        self.caller_ceiling = ceiling;
        self
    }

    /// A fresh instance sharing this one's live `engine`/`audit` handles
    /// (the same shared SOP engine, not a rebuild — there is only one), bound
    /// to `ceiling`. Used by the `Bounded` delegate rebuild in `delegate.rs`,
    /// mirroring `McpToolWrapper::rebound` for a resource that cannot be
    /// reconstructed from config.
    pub(crate) fn rebound_with_ceiling(
        &self,
        ceiling: crate::tools::caller_ceiling::CallerCeiling,
    ) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            audit: self.audit.clone(),
            caller_ceiling: Some(ceiling),
            // Deliberately not the caller's initiator. The initiator lets the
            // headless driver resume a parked run as that agent, with its full
            // policy and no ceiling. Bounded runs are cancelled before they can
            // park; if a cancellation ever fails, an unowned step must fail
            // closed at resume rather than run as the caller.
            initiator: None,
        }
    }
}

#[async_trait]
impl Tool for SopExecuteTool {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn name(&self) -> &str {
        "sop_execute"
    }

    fn description(&self) -> &str {
        "Manually trigger a Standard Operating Procedure (SOP) by name. Returns the run ID and first step instruction. Use sop_list to see available SOPs."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the SOP to execute"
                },
                "payload": {
                    "type": "string",
                    "description": "Optional trigger payload (JSON string)"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let sop_name = args.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "name"})),
                "tool argument validation failed"
            );

            anyhow::Error::msg("Missing 'name' parameter")
        })?;

        let payload = args
            .get("payload")
            .and_then(|v| v.as_str())
            .map(String::from);

        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload,
            timestamp: now_iso8601(),
        };

        // Lock engine, start run, snapshot run for audit, then drop lock
        let (action, run_snapshot) = {
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

            match engine.start_run_owned(sop_name, event, self.initiator.as_deref()) {
                Ok(action) => {
                    // A bounded caller must never leave a run parked: nothing
                    // in ITS turn will ever drive it past this point (see
                    // `executor::parked_run_id`), and an external approver has
                    // no way to know the run was started under a sealed
                    // ceiling before granting the step agent full authority.
                    // Cancel it under the SAME engine lock this action came
                    // from, so no concurrent resolve can land in between.
                    if self.caller_ceiling.is_some()
                        && let Some(run_id) = crate::sop::executor::parked_run_id(&action)
                    {
                        let run_id = run_id.to_string();
                        // `cancel_run` persists the terminal record BEFORE
                        // removing the run from `active_runs` (`finish_run_
                        // with_gate_event`), so a persistence failure here
                        // leaves the run genuinely still active. Reporting
                        // "has been cancelled" regardless would tell the
                        // caller the escape was closed when it was not -
                        // report the cancellation failure itself instead.
                        if let Err(e) = engine.cancel_run(&run_id) {
                            ::zeroclaw_log::record!(
                                ERROR,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Fail
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "run_id": run_id,
                                    "error": e.to_string(),
                                })),
                                "bounded sop_execute: parked run could not be cancelled"
                            );
                            return Ok(ToolResult {
                                success: false,
                                output: ToolOutput::default(),
                                error: Some(format!(
                                    "sop_execute: refused — SOP '{sop_name}' left outside this \
                                     turn (parked or pending on a dependency), which a bounded \
                                     caller's tool ceiling cannot follow past this turn; run \
                                     {run_id} could NOT be cancelled ({e}) and may still be \
                                     active - treat it as unresolved, not closed"
                                )),
                            });
                        }
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(format!(
                                "sop_execute: refused — SOP '{sop_name}' left outside this turn \
                                 (parked or pending on a dependency), which a bounded caller's \
                                 tool ceiling cannot follow past this turn; run {run_id} has \
                                 been cancelled"
                            )),
                        });
                    }
                    let run_id = action_run_id(&action);
                    let snapshot = run_id.and_then(|id| engine.get_run(id).cloned());
                    (Ok(action), snapshot)
                }
                Err(e) => (Err(e), None),
            }
        };

        // Audit log (engine lock dropped, safe to await)
        if let Some(ref audit) = self.audit
            && let Some(ref run) = run_snapshot
            && let Err(e) = audit.log_run_start(run).await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "SOP audit log_run_start failed"
            );
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
                let output = match action {
                    SopRunAction::ExecuteStep {
                        run_id, context, ..
                    } => {
                        format!("SOP run started: {run_id}\n\n{context}")
                    }
                    SopRunAction::WaitApproval {
                        run_id, context, ..
                    } => {
                        format!("SOP run started: {run_id} (waiting for approval)\n\n{context}")
                    }
                    SopRunAction::Completed { run_id, sop_name } => {
                        format!("SOP '{sop_name}' run {run_id} completed immediately (no steps).")
                    }
                    SopRunAction::Cancelled { run_id, sop_name } => {
                        format!("SOP '{sop_name}' run {run_id} was cancelled.")
                    }
                    SopRunAction::Failed { run_id, reason, .. } => {
                        format!("SOP run {run_id} failed: {reason}")
                    }
                    SopRunAction::DeterministicStep { run_id, step, .. } => {
                        format!(
                            "SOP run started (deterministic): {run_id}\nFirst step: {}",
                            step.title
                        )
                    }
                    SopRunAction::CheckpointWait { run_id, step, .. } => {
                        format!(
                            "SOP run started: {run_id} (paused at checkpoint: {})",
                            step.title
                        )
                    }
                    SopRunAction::Pending {
                        run_id,
                        step,
                        reason,
                        ..
                    } => {
                        format!("SOP run {run_id} pending before step {step}: {reason}")
                    }
                };
                Ok(ToolResult {
                    success: true,
                    output: output.into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to start SOP: {e}")),
            }),
        }
    }
}

/// Extract run_id from any SopRunAction variant.
fn action_run_id(action: &SopRunAction) -> Option<&str> {
    match action {
        SopRunAction::ExecuteStep { run_id, .. }
        | SopRunAction::WaitApproval { run_id, .. }
        | SopRunAction::Completed { run_id, .. }
        | SopRunAction::Cancelled { run_id, .. }
        | SopRunAction::Failed { run_id, .. }
        | SopRunAction::DeterministicStep { run_id, .. }
        | SopRunAction::CheckpointWait { run_id, .. }
        | SopRunAction::Pending { run_id, .. } => Some(run_id),
    }
}

use crate::sop::engine::now_iso8601;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::engine::SopEngine;
    use crate::sop::types::*;
    use zeroclaw_config::schema::SopConfig;

    fn test_sop(name: &str, mode: SopExecutionMode) -> Sop {
        Sop {
            name: name.into(),
            description: format!("Test SOP: {name}"),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
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
            decision: None,
        }
    }

    /// A `Deterministic`-mode SOP whose FIRST step is `kind: Checkpoint`, so
    /// `start_run` reaches `CheckpointWait` directly (`activate_reserved_run`
    /// dispatches to `dispatch_deterministic_step` whenever `execution_mode ==
    /// Deterministic`, and `resolve_deterministic_action`'s `Checkpoint` arm
    /// needs nothing else — `engine.rs:1910-1922`, `:4860-4939`). `checkpoint`
    /// selects which of the two park variants `parked_run_id` sees first;
    /// `false` reuses `test_sop`'s two plain `Execute` steps instead, so the
    /// same fixture also gives the non-parking Deterministic control.
    fn deterministic_sop(name: &str, checkpoint_first: bool) -> Sop {
        let mut sop = test_sop(name, SopExecutionMode::Deterministic);
        sop.deterministic = true;
        if checkpoint_first {
            sop.steps[0].kind = SopStepKind::Checkpoint;
        }
        sop
    }

    /// Step one declares a dependency on a step number that does not exist, so
    /// `route::eligible` (`sop/route/mod.rs:141-146`) is permanently false and
    /// `start_run` returns `Pending` right away — reachable from ANY
    /// `execution_mode` (`dispatch_llm_step` and `resolve_deterministic_action`
    /// both check it before anything mode-specific). `Auto` here only because
    /// it is the plainest mode; the dependency check runs ahead of it.
    fn sop_with_unmet_dependency(name: &str) -> Sop {
        let mut sop = test_sop(name, SopExecutionMode::Auto);
        sop.steps[0].routing.depends_on = vec![99];
        sop
    }

    fn engine_with_sops(sops: Vec<Sop>) -> Arc<Mutex<SopEngine>> {
        let mut engine = SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(sops);
        Arc::new(Mutex::new(engine))
    }

    #[tokio::test]
    async fn execute_auto_sop() {
        let engine = engine_with_sops(vec![test_sop("test-sop", SopExecutionMode::Auto)]);
        let tool = SopExecuteTool::new(engine);
        let result = tool.execute(json!({"name": "test-sop"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("run-"));
        assert!(result.output.contains("Step one"));
    }

    #[tokio::test]
    async fn execute_supervised_sop() {
        let engine = engine_with_sops(vec![test_sop("test-sop", SopExecutionMode::Supervised)]);
        let tool = SopExecuteTool::new(engine);
        let result = tool.execute(json!({"name": "test-sop"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("waiting for approval"));
    }

    // ── bounded ceiling vs. a run that parks ────────────────────────────────

    fn sealed_ceiling(names: &[&str]) -> crate::tools::caller_ceiling::CallerCeiling {
        let handle: crate::tools::caller_ceiling::CallerCeiling =
            Arc::new(std::sync::OnceLock::new());
        let _ = handle.set(names.iter().map(|n| (*n).to_string()).collect());
        handle
    }

    /// The mechanism: a bounded caller's `sop_execute` shares the same live
    /// engine as the unbounded owner (`SAFE_FOR_BOUNDED_REUSE`); the run this
    /// call parks would otherwise sit resolvable by anyone, and an external
    /// approval later runs the step through `agent::run` with
    /// `allowed_tools: None` — full authority, outside this ceiling. Positive
    /// control: `execute_supervised_sop` above is the identical scenario with
    /// no ceiling, and it parks normally.
    #[tokio::test]
    async fn execute_supervised_sop_under_ceiling_is_refused_and_the_run_cannot_be_approved() {
        let engine = engine_with_sops(vec![test_sop("test-sop", SopExecutionMode::Supervised)]);
        let tool = SopExecuteTool::new(Arc::clone(&engine))
            .with_caller_ceiling(Some(sealed_ceiling(&["sop_execute"])));

        let result = tool.execute(json!({"name": "test-sop"})).await.unwrap();

        assert!(
            !result.success,
            "a bounded caller must not be able to leave a run parked: {result:?}"
        );
        let error = result.error.clone().unwrap_or_default();
        assert!(
            error.contains("refused") && error.contains("has been cancelled"),
            "refusal must name the cancellation, got: {error}"
        );
        let run_id = error
            .split("run ")
            .nth(1)
            .and_then(|s| s.split(" has been cancelled").next())
            .expect("the refusal names the cancelled run")
            .to_string();

        let mut engine = engine.lock().unwrap();
        assert!(
            !engine.active_runs().contains_key(&run_id),
            "the parked run must not remain active"
        );
        assert_eq!(
            engine.get_run(&run_id).map(|r| r.status),
            Some(SopRunStatus::Cancelled),
            "the parked run must have been cancelled, not merely hidden from the caller"
        );
        let outcome = engine
            .resolve_gate(
                &run_id,
                crate::sop::approval::ApprovalDecision::Approve,
                crate::sop::approval::ApprovalPrincipal::cli(None),
            )
            .expect("resolving a cancelled run's gate must not error");
        // Ground truth from `SopEngine::gate_state` (engine.rs): ANY run present
        // in `finished_runs` (Cancelled included) maps unconditionally to
        // `GateState::AlreadyResolved`, never `Waiting` - `resolve_gate` treats
        // it as a permanent idempotent no-op. This corrects an earlier, wrong
        // prediction of `NotWaiting` (that variant is for a run absent
        // entirely, not a terminal one); the property that actually matters -
        // never `Resumed` - holds under the observed variant too.
        assert!(
            !matches!(outcome, crate::sop::approval::ResolveOutcome::Resumed(_)),
            "an external approver must not be able to resume the cancelled run into a \
             fresh step, got: {outcome:?}"
        );
    }

    /// Regression: `engine.cancel_run(&run_id)`'s `Err` branch above only logs
    /// the failure - the very next statement still returns the SAME "has been
    /// cancelled" refusal text regardless. `cancel_run` -> `finish_run_with_gate_event`
    /// persists the terminal record BEFORE removing the run from `active_runs`
    /// (engine.rs), so a persistence failure leaves the run genuinely still
    /// active while the caller is told it was cancelled. The three positive
    /// tests above (this one, `..._checkpoint_...`, `..._unmet_dependency_...`)
    /// are this test's own positive control: same scenario, a store that
    /// actually persists, and they already assert the message DOES say
    /// "has been cancelled" - so this fix must not make cancellation reporting
    /// pessimistic in the common case, only honest in the failure case.
    #[tokio::test]
    async fn execute_supervised_sop_under_ceiling_reports_persistence_failure_instead_of_false_success()
     {
        let mut engine = SopEngine::new(SopConfig::default()).with_store(Arc::new(
            crate::sop::test_support::AlwaysFailFinishStore::new(),
        ));
        engine.set_sops_for_test(vec![test_sop("test-sop", SopExecutionMode::Supervised)]);
        let engine = Arc::new(Mutex::new(engine));
        let tool = SopExecuteTool::new(Arc::clone(&engine))
            .with_caller_ceiling(Some(sealed_ceiling(&["sop_execute"])));

        let result = tool.execute(json!({"name": "test-sop"})).await.unwrap();

        assert!(
            !result.success,
            "a bounded caller must still be refused even when cancellation itself fails: \
             {result:?}"
        );
        let error = result.error.clone().unwrap_or_default();
        assert!(
            !error.contains("has been cancelled"),
            "regression: the refusal must not claim the run was cancelled when the \
             engine's own cancel_run call failed (persistence error) - got: {error}"
        );

        let run_id = engine
            .lock()
            .unwrap()
            .active_runs()
            .keys()
            .next()
            .cloned()
            .expect("the run must still be active: cancellation never persisted");
        assert_eq!(
            engine.lock().unwrap().get_run(&run_id).map(|r| r.status),
            Some(SopRunStatus::WaitingApproval),
            "the run must still show its pre-cancellation status, not a terminal one that \
             was never actually persisted"
        );
    }

    /// The path this fix must NOT touch: a fully automatic SOP never parks, so
    /// it stays inside the live in-turn `SopStepReassembly` machinery that
    /// `bounded_delegate_sop_step_ceiling` already covers. A bounded caller
    /// must still be able to run it — a blanket refusal under any ceiling
    /// (rejected in favor of this narrower fix) would have broken exactly this.
    #[tokio::test]
    async fn execute_auto_sop_under_ceiling_still_succeeds() {
        let engine = engine_with_sops(vec![test_sop("test-sop", SopExecutionMode::Auto)]);
        let tool =
            SopExecuteTool::new(engine).with_caller_ceiling(Some(sealed_ceiling(&["sop_execute"])));

        let result = tool.execute(json!({"name": "test-sop"})).await.unwrap();

        assert!(
            result.success,
            "a SOP that never parks must not be refused under a ceiling: {result:?}"
        );
        assert!(result.output.contains("Step one"));
    }

    /// The initiator is what lets the headless driver resume a parked run as
    /// its starting agent, with that agent's full policy and no ceiling. A
    /// bounded caller's run must never be resumable that way, so the rebound
    /// instance records no initiator: if a park ever survives (cancellation
    /// failed), an unowned step fails closed at resume instead of running as
    /// the caller. Control: the unbounded instance still records it.
    #[tokio::test]
    async fn rebound_with_ceiling_does_not_carry_the_initiator() {
        let engine = engine_with_sops(vec![
            test_sop("unbounded-sop", SopExecutionMode::Auto),
            test_sop("bounded-sop", SopExecutionMode::Auto),
        ]);
        let unbounded = SopExecuteTool::new(Arc::clone(&engine)).with_initiator("caller");
        let bounded = unbounded.rebound_with_ceiling(sealed_ceiling(&["sop_execute"]));

        let control = unbounded
            .execute(json!({"name": "unbounded-sop"}))
            .await
            .unwrap();
        assert!(control.success, "{control:?}");
        let result = bounded
            .execute(json!({"name": "bounded-sop"}))
            .await
            .unwrap();
        assert!(result.success, "{result:?}");

        let engine = engine.lock().unwrap();
        let mut initiators: Vec<Option<String>> = engine
            .active_runs()
            .values()
            .map(|run| run.initiating_agent.clone())
            .collect();
        initiators.sort();
        assert_eq!(
            initiators,
            vec![None, Some("caller".to_string())],
            "the unbounded run records its initiator; the bounded one must not"
        );
    }

    /// The other park variant `parked_run_id` matches. Found by re-reading the
    /// dispatch code rather than assuming: `CheckpointWait` was believed
    /// unreachable from `sop_execute` (thought to be exclusive to the headless
    /// deterministic driver) until tracing `activate_reserved_run` showed
    /// `start_run` itself dispatches deterministically whenever
    /// `execution_mode == Deterministic`, with no other precondition on the
    /// `Checkpoint` arm. Same shape as the `WaitApproval` test above, plus the
    /// checkpoint-specific resolution path: `approve_step` (not `resolve_gate`,
    /// which is scoped to `WaitingApproval`) errors on a cancelled run because
    /// it is no longer in `active_runs` at all — a different shape of "cannot
    /// resume" than `resolve_gate`'s `AlreadyResolved`, not assumed identical.
    #[tokio::test]
    async fn execute_deterministic_checkpoint_sop_under_ceiling_is_refused_and_the_run_cannot_be_approved()
     {
        let engine = engine_with_sops(vec![deterministic_sop("det-sop", true)]);
        let tool = SopExecuteTool::new(Arc::clone(&engine))
            .with_caller_ceiling(Some(sealed_ceiling(&["sop_execute"])));

        let result = tool.execute(json!({"name": "det-sop"})).await.unwrap();

        assert!(
            !result.success,
            "a bounded caller must not be able to leave a checkpoint-parked run: {result:?}"
        );
        let error = result.error.clone().unwrap_or_default();
        assert!(
            error.contains("refused") && error.contains("has been cancelled"),
            "refusal must name the cancellation, got: {error}"
        );
        let run_id = error
            .split("run ")
            .nth(1)
            .and_then(|s| s.split(" has been cancelled").next())
            .expect("the refusal names the cancelled run")
            .to_string();

        let mut engine = engine.lock().unwrap();
        assert!(!engine.active_runs().contains_key(&run_id));
        assert_eq!(
            engine.get_run(&run_id).map(|r| r.status),
            Some(SopRunStatus::Cancelled)
        );
        let resumed = engine.approve_step(&run_id);
        assert!(
            resumed.is_err(),
            "an external approver must not be able to resume the cancelled checkpoint, got: \
             {resumed:?}"
        );
    }

    /// Positive control for the test above: `Deterministic` mode itself is not
    /// what triggers a refusal — only actually parking does. Step one here is
    /// the default `Execute` kind (`checkpoint_first: false`), so `start_run`
    /// returns `DeterministicStep`, which `parked_run_id` does not match.
    #[tokio::test]
    async fn execute_deterministic_non_checkpoint_sop_under_ceiling_still_succeeds() {
        let engine = engine_with_sops(vec![deterministic_sop("det-sop", false)]);
        let tool =
            SopExecuteTool::new(engine).with_caller_ceiling(Some(sealed_ceiling(&["sop_execute"])));

        let result = tool.execute(json!({"name": "det-sop"})).await.unwrap();

        assert!(
            result.success,
            "a deterministic run that does not park must not be refused under a ceiling: \
             {result:?}"
        );
        assert!(result.output.contains("deterministic"));
    }

    /// The third, least obvious way `parked_run_id` must catch a run: `Pending`
    /// on an unmet dependency looks nothing like an approval park from here, but
    /// `SopEngine::run_maintenance_tick` can promote a gated pending run to a
    /// real park later, on a background tick with no caller or ceiling at all
    /// (`engine.rs`, `retry_capacity_blocked_gated_pends`). Found by re-deriving
    /// the mechanism ("anything `enqueue_live_action` does not enqueue escapes
    /// the turn") instead of trusting the two variants already handled.
    #[tokio::test]
    async fn execute_sop_with_unmet_dependency_under_ceiling_is_refused_and_the_run_cannot_be_approved()
     {
        let engine = engine_with_sops(vec![sop_with_unmet_dependency("det-sop")]);
        let tool = SopExecuteTool::new(Arc::clone(&engine))
            .with_caller_ceiling(Some(sealed_ceiling(&["sop_execute"])));

        let result = tool.execute(json!({"name": "det-sop"})).await.unwrap();

        assert!(
            !result.success,
            "a bounded caller must not be able to leave a dependency-pending run active: \
             {result:?}"
        );
        let error = result.error.clone().unwrap_or_default();
        assert!(
            error.contains("refused") && error.contains("has been cancelled"),
            "refusal must name the cancellation, got: {error}"
        );
        let run_id = error
            .split("run ")
            .nth(1)
            .and_then(|s| s.split(" has been cancelled").next())
            .expect("the refusal names the cancelled run")
            .to_string();

        let engine = engine.lock().unwrap();
        assert!(!engine.active_runs().contains_key(&run_id));
        assert_eq!(
            engine.get_run(&run_id).map(|r| r.status),
            Some(SopRunStatus::Cancelled)
        );
    }

    #[tokio::test]
    async fn execute_unknown_sop() {
        let engine = engine_with_sops(vec![]);
        let tool = SopExecuteTool::new(engine);
        let result = tool.execute(json!({"name": "nonexistent"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Failed to start SOP"));
    }

    #[tokio::test]
    async fn execute_missing_name() {
        let engine = engine_with_sops(vec![]);
        let tool = SopExecuteTool::new(engine);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_with_payload() {
        let engine = engine_with_sops(vec![test_sop("test-sop", SopExecutionMode::Auto)]);
        let tool = SopExecuteTool::new(engine);
        let result = tool
            .execute(json!({"name": "test-sop", "payload": "{\"value\": 87.3}"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("87.3"));
    }

    #[test]
    fn name_and_schema() {
        let engine = engine_with_sops(vec![]);
        let tool = SopExecuteTool::new(engine);
        assert_eq!(tool.name(), "sop_execute");
        assert!(tool.parameters_schema()["required"].is_array());
    }
}
