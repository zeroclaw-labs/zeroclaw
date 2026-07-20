use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use crate::sop::approval::{ApprovalDecision, ApprovalPrincipal, BrokerOutcome, ResolveOutcome};
use crate::sop::types::{SopRunAction, SopRunStatus};
use crate::sop::{SopAuditLogger, SopEngine};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

/// Approve a pending SOP step that is waiting for operator approval.
pub struct SopApproveTool {
    engine: Arc<Mutex<SopEngine>>,
    audit: Option<Arc<SopAuditLogger>>,
    agent_alias: String,
}

impl SopApproveTool {
    pub fn new(engine: Arc<Mutex<SopEngine>>) -> Self {
        Self {
            engine,
            audit: None,
            agent_alias: "agent".to_string(),
        }
    }

    pub fn with_audit(mut self, audit: Arc<SopAuditLogger>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Set the agent alias recorded as the approval principal (default `"agent"`).
    pub fn with_agent_alias(mut self, alias: impl Into<String>) -> Self {
        self.agent_alias = alias.into();
        self
    }
}

#[async_trait]
impl Tool for SopApproveTool {
    fn name(&self) -> &str {
        "sop_approve"
    }

    fn description(&self) -> &str {
        "Approve a pending SOP step that is waiting for operator approval. Returns the step instruction to execute. Use sop_status to see which runs are waiting."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "The run ID to approve"
                }
            },
            "required": ["run_id"]
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

        let result = {
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

            // EPIC G: route through the broker (membership + quorum). With no
            // `[sop.approval]` policy it is exactly `resolve_gate`, so behavior is
            // unchanged; with a policy the agent must be an authorized member and a
            // quorum must be met before the chokepoint clears the gate.
            match engine.resolve_via_broker(
                run_id,
                ApprovalDecision::Approve,
                ApprovalPrincipal::agent(&self.agent_alias),
            ) {
                // A deterministic checkpoint is an in-band agent pause, not an
                // out-of-band gate, so the broker reports NotWaiting; resume it
                // through approve_step (the checkpoint owner).
                Ok(BrokerOutcome::NotWaiting)
                | Ok(BrokerOutcome::Resolved(ResolveOutcome::NotWaiting)) => {
                    let is_checkpoint = matches!(
                        engine.get_run(run_id).map(|r| r.status),
                        Some(SopRunStatus::PausedCheckpoint)
                    );
                    if is_checkpoint {
                        engine.approve_step(run_id).map(|action| {
                            BrokerOutcome::Resolved(ResolveOutcome::Resumed(Box::new(action)))
                        })
                    } else {
                        Ok(BrokerOutcome::NotWaiting)
                    }
                }
                other => other,
            }
        };

        match result {
            Ok(BrokerOutcome::Resolved(ResolveOutcome::Resumed(action))) => {
                crate::sop::executor::enqueue_live_action(
                    Arc::clone(&self.engine),
                    self.audit.clone(),
                    &action,
                );
                let output = match *action {
                    SopRunAction::ExecuteStep {
                        run_id, context, ..
                    } => {
                        format!("Approved. Proceeding with run {run_id}.\n\n{context}")
                    }
                    other => format!("Approved. Action: {other:?}"),
                };
                Ok(ToolResult {
                    success: true,
                    output: output.into(),
                    error: None,
                })
            }
            Ok(BrokerOutcome::Resolved(ResolveOutcome::RejectedSelfApproval)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(
                    "This SOP gate requires an out-of-band approver \
                     (approval_mode = out_of_band_required). Use `zeroclaw sop approve <run_id>` \
                     or the dashboard."
                        .to_string(),
                ),
            }),
            Ok(BrokerOutcome::Resolved(ResolveOutcome::AlreadyResolved)) => Ok(ToolResult {
                success: true,
                output: format!("Run {run_id} was already resolved.").into(),
                error: None,
            }),
            Ok(BrokerOutcome::Resolved(ResolveOutcome::Denied)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Run {run_id} was denied.")),
            }),
            Ok(BrokerOutcome::Resolved(ResolveOutcome::NotWaiting))
            | Ok(BrokerOutcome::NotWaiting) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Approval failed: run {run_id} is not waiting for approval."
                )),
            }),
            // A quorum can record a valid vote without clearing the gate yet.
            Ok(BrokerOutcome::PendingQuorum { have, need }) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Approval recorded ({have} of {need}). Awaiting {} more distinct approver(s).",
                    need.saturating_sub(have)
                )
                .into(),
                error: None,
            }),
            Ok(BrokerOutcome::NotAuthorized { required_group }) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Not authorized: approving this step requires membership in the \
                     '{required_group}' group."
                )),
            }),
            // Fail closed: the step names an approval policy absent from config, so
            // the gate is left waiting rather than cleared.
            Ok(BrokerOutcome::PolicyMissing { name }) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Approval failed: step names approval policy '{name}', which is not \
                     defined in [sop.approval].policies; the gate is left waiting."
                )),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Approval failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::engine::SopEngine;
    use crate::sop::types::*;
    use zeroclaw_config::schema::SopConfig;

    fn test_sop() -> Sop {
        Sop {
            name: "test-sop".into(),
            description: "Test SOP".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "Step one".into(),
                body: "Do it".into(),
                suggested_tools: vec![],
                requires_confirmation: false,
                kind: SopStepKind::default(),
                schema: None,
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        }
    }

    fn engine_with_run() -> (Arc<Mutex<SopEngine>>, String) {
        let mut engine = SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(vec![test_sop()]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-02-19T12:00:00Z".into(),
        };
        // Start run — Supervised mode → WaitApproval
        engine.start_run("test-sop", event).unwrap();
        let run_id = engine
            .active_runs()
            .keys()
            .next()
            .expect("expected active run")
            .clone();
        (Arc::new(Mutex::new(engine)), run_id)
    }

    #[tokio::test]
    async fn approve_waiting_run() {
        let (engine, run_id) = engine_with_run();
        let tool = SopApproveTool::new(engine);
        let result = tool.execute(json!({"run_id": run_id})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Approved"));
        assert!(result.output.contains("Step one"));
    }

    #[tokio::test]
    async fn approve_nonexistent_run() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tool = SopApproveTool::new(engine);
        let result = tool
            .execute(json!({"run_id": "nonexistent"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Approval failed"));
    }

    #[tokio::test]
    async fn approve_missing_run_id() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tool = SopApproveTool::new(engine);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn name_and_schema() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tool = SopApproveTool::new(engine);
        assert_eq!(tool.name(), "sop_approve");
        assert!(tool.parameters_schema()["required"].is_array());
    }

    #[tokio::test]
    async fn agent_approve_rejected_under_out_of_band_required() {
        // The agent tool cannot self-satisfy a gate under out_of_band_required; the
        // gate stays open for a CLI/gateway approver.
        let cfg = SopConfig {
            approval_mode: zeroclaw_config::schema::ApprovalMode::OutOfBandRequired,
            ..SopConfig::default()
        };
        let mut engine = SopEngine::new(cfg);
        engine.set_sops_for_test(vec![test_sop()]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-02-19T12:00:00Z".into(),
        };
        engine.start_run("test-sop", event).unwrap();
        let run_id = engine.active_runs().keys().next().unwrap().clone();

        let tool = SopApproveTool::new(Arc::new(Mutex::new(engine)));
        let result = tool.execute(json!({ "run_id": run_id })).await.unwrap();
        assert!(!result.success, "agent self-approval is rejected");
        assert!(
            result.error.unwrap().contains("out-of-band"),
            "message points the agent at the out-of-band approver"
        );
    }

    #[tokio::test]
    async fn approve_writes_append_only_ledger() {
        // C5: the audit of record is the append-only store ledger, written inside
        // resolve_gate with the principal - not the legacy Memory overwrite.
        let (engine, run_id) = engine_with_run();
        let tool = SopApproveTool::new(engine.clone());
        let result = tool.execute(json!({ "run_id": &run_id })).await.unwrap();
        assert!(result.success);

        let events = engine.lock().unwrap().run_events(&run_id).unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.kind == "gate_resolved" && e.actor.as_deref() == Some("agent")),
            "approval writes an append-only gate_resolved ledger row attributed to the agent"
        );
    }
}
