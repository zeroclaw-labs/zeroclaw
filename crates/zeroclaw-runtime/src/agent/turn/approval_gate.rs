//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::approval::{ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use crate::control_plane::{
    GoalBlockerKind, current_goal_admission_context, current_goal_turn_evaluation_requested,
    pause_current_goal_for_human_gate,
};
use std::time::Duration;

pub(crate) enum ApprovalGateOutcome {
    Proceed { approved: bool },
    Deny(ToolExecutionOutcome),
    Replace(ToolExecutionOutcome),
    Cancel,
}

/// Run the approval flow for one tool call (upstream loop body, approval
/// section): resolve the tool's approval requirement, prompt interactively on
/// CLI or via the channel's inline approval on non-interactive channels
/// (falling back to auto-deny), and record the decision.
pub(crate) async fn gate_tool_approval(
    ctx: &TurnCtx<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    iteration: usize,
) -> ApprovalGateOutcome {
    let mut approval_requirement = ctx
        .approval
        .map(|mgr| mgr.approval_requirement(tool_name))
        .unwrap_or(ApprovalRequirement::NotRequired);
    if let Some(mgr) = ctx.approval
        && approval_requirement == ApprovalRequirement::Prompt
    {
        let request = ApprovalRequest {
            tool_name: tool_name.to_string(),
            arguments: tool_args.clone(),
        };
        if pause_goal_for_tool_approval(ctx, &request).await {
            return ApprovalGateOutcome::Cancel;
        }

        // Interactive CLI: prompt the operator.
        // Non-interactive (channels): try the channel's inline
        // approval (e.g. Telegram inline keyboard) before falling
        // back to auto-deny.
        let (decision, decided_by) = if mgr.is_non_interactive() {
            let attributed = if let Some(ch) = ctx.channel {
                let ch_request = zeroclaw_api::channel::ChannelApprovalRequest {
                    tool_name: request.tool_name.clone(),
                    arguments_summary: crate::approval::summarize_args(&request.arguments),
                    raw_arguments: Some(request.arguments.clone()),
                };
                let recipient = ctx.channel_reply_target.unwrap_or_default();
                match ch.request_approval_attributed(recipient, &ch_request).await {
                    Ok(Some(a)) => Some(a),
                    Ok(None) => None,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_category(::zeroclaw_log::EventCategory::Tool)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Channel approval request failed"
                        );
                        None
                    }
                }
            } else {
                None
            };
            // The deciding back-channel (when a fan-out bridge answered) rides
            // back on the response itself, so attribution can't be cross-wired
            // by a concurrent approval on the same channel instance.
            let decided_by = attributed.as_ref().and_then(|a| a.decided_by.clone());
            let decision = match attributed.map(|a| a.response) {
                Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve) => {
                    ApprovalResponse::Yes
                }
                Some(zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove) => {
                    ApprovalResponse::Always
                }
                Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny) => ApprovalResponse::No,
                Some(zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit {
                    replacement,
                }) => ApprovalResponse::ReplaceWith(replacement),
                // Channel doesn't support approval — auto-deny.
                None => ApprovalResponse::No,
            };
            (decision, decided_by)
        } else {
            (mgr.prompt_cli(&request), None)
        };

        let decision_channel = decided_by.unwrap_or_else(|| ctx.channel_name.to_string());
        mgr.record_decision(tool_name, tool_args, &decision, &decision_channel);

        if decision == ApprovalResponse::No {
            let denied = "Denied by user.".to_string();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_category(::zeroclaw_log::EventCategory::Tool)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "model": ctx.model,
                        "iteration": iteration + 1,
                        "tool": tool_name,
                        "arguments": scrub_credentials(&tool_args.to_string()),
                        "result": denied,
                        "trace_id": ctx.turn_id,
                    })),
                "tool_call_result"
            );
            if let Some(tx) = ctx.on_delta {
                let _ = tx
                    .send(StreamDelta::Status(format!(
                        "\u{274c} {}: {}\n",
                        tool_name, denied
                    )))
                    .await;
            }
            return ApprovalGateOutcome::Deny(ToolExecutionOutcome {
                output: denied.clone(),
                success: false,
                error_reason: Some(denied),
                duration: Duration::ZERO,
                receipt: None,
                output_data: None,
            });
        }

        if let ApprovalResponse::ReplaceWith(replacement) = &decision {
            if let Some(tx) = ctx.on_delta {
                let _ = tx
                    .send(StreamDelta::Status(format!(
                        "\u{270f} {}: replaced by user\n",
                        tool_name
                    )))
                    .await;
            }
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Approve)
                    .with_category(::zeroclaw_log::EventCategory::Tool)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "model": ctx.model,
                        "iteration": iteration + 1,
                        "tool": tool_name,
                        "arguments": scrub_credentials(&tool_args.to_string()),
                        "replaced": true,
                        "output": scrub_credentials(replacement),
                        "trace_id": ctx.turn_id,
                    })),
                "tool_call_result"
            );
            return ApprovalGateOutcome::Replace(ToolExecutionOutcome {
                output: crate::approval::sanitize_tool_replacement(replacement),
                success: true,
                error_reason: None,
                duration: Duration::ZERO,
                receipt: None,
                output_data: None,
            });
        }

        if matches!(decision, ApprovalResponse::Yes | ApprovalResponse::Always) {
            approval_requirement = ApprovalRequirement::Approved;
        }
    }

    ApprovalGateOutcome::Proceed {
        approved: approval_requirement == ApprovalRequirement::Approved,
    }
}

async fn pause_goal_for_tool_approval(ctx: &TurnCtx<'_>, request: &ApprovalRequest) -> bool {
    if !current_goal_turn_evaluation_requested() {
        return false;
    }
    let Some(goal_ctx) = current_goal_admission_context() else {
        return false;
    };
    let arguments_summary = crate::approval::summarize_args(&request.arguments);
    let message = crate::i18n::get_required_cli_string_with_args(
        "goal-command-tool-approval-required-blocker",
        &[("tool", &request.tool_name)],
    );
    let payload = serde_json::json!({
        "tool": request.tool_name.as_str(),
        "arguments_summary": arguments_summary,
    });
    match pause_current_goal_for_human_gate(
        &goal_ctx,
        None,
        GoalBlockerKind::NeedsUserInput,
        message,
        Some(payload),
    )
    .await
    {
        Ok(Some(_)) => {
            if let Some(tx) = ctx.on_delta {
                let _ = tx
                    .send(StreamDelta::Status(
                        crate::i18n::get_required_cli_string_with_args(
                            "goal-command-tool-approval-required-status",
                            &[("tool", &request.tool_name)],
                        ),
                    ))
                    .await;
            }
            true
        }
        Ok(None) => false,
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Tool)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "tool": request.tool_name.as_str(),
                        "error": format!("{error:#}"),
                    })),
                "goal approval blocker pause failed"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::noop::NoopObserver;
    use crate::{control_plane, i18n};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig};

    fn ensure_test_control_plane() -> (
        Arc<dyn control_plane::TaskRegistry>,
        Arc<dyn control_plane::GoalTaskRegistry>,
    ) {
        match control_plane::control_plane() {
            Some(control_plane) => (
                Arc::clone(&control_plane.store),
                Arc::clone(&control_plane.goal_store),
            ),
            None => {
                let sqlite_store =
                    Arc::new(control_plane::SqliteTaskStore::new_in_memory().unwrap());
                let store: Arc<dyn control_plane::TaskRegistry> = sqlite_store.clone();
                let goal_store: Arc<dyn control_plane::GoalTaskRegistry> = sqlite_store;
                let _ = control_plane::init_control_plane(control_plane::ControlPlaneHandle {
                    store: Arc::clone(&store),
                    goal_store: Arc::clone(&goal_store),
                    boot_id: "test-boot".into(),
                    recovered_goal_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
                    data_dir_lock: None,
                });
                (
                    Arc::clone(&control_plane::control_plane().unwrap().store),
                    Arc::clone(&control_plane::control_plane().unwrap().goal_store),
                )
            }
        }
    }

    async fn create_running_goal() -> (
        Arc<dyn control_plane::TaskRegistry>,
        Arc<dyn control_plane::GoalTaskRegistry>,
        String,
        control_plane::GoalAdmissionContext,
    ) {
        let (store, goal_store) = ensure_test_control_plane();
        let task_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent = format!("agent-{}", uuid::Uuid::new_v4());
        let route = format!("route-{}", uuid::Uuid::new_v4());
        let principal = format!("principal-{}", uuid::Uuid::new_v4());
        let ctx = control_plane::GoalAdmissionContext::new(agent.clone())
            .with_channel_type(Some("matrix".into()))
            .with_originator_route(Some(route.clone()))
            .with_principal_id(Some(principal.clone()));
        goal_store
            .create_goal(
                control_plane::TaskRecord {
                    id: task_id.clone(),
                    kind: control_plane::TaskKind::Goal,
                    agent,
                    status: control_plane::TaskStatus::Running,
                    owner_pid: std::process::id(),
                    owner_boot_id: "test-boot".into(),
                    heartbeat_at: None,
                    depth: 0,
                    parent_id: None,
                    originator_route: Some(route),
                    delivered: false,
                    idem_key: None,
                    principal_id: Some(principal),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    finished_at: None,
                },
                control_plane::GoalTaskRecord {
                    task_id: task_id.clone(),
                    objective: "wait for approval".into(),
                    effective_token_limit: None,
                    effective_cost_limit_usd: None,
                    pause_reason: None,
                    pause_description: None,
                    blockers: Vec::new(),
                },
                None,
            )
            .await
            .unwrap();
        (store, goal_store, task_id, ctx)
    }

    #[tokio::test]
    async fn approval_prompt_in_goal_pauses_goal_and_cancels_turn() {
        let (store, goal_store, task_id, goal_ctx) = create_running_goal().await;
        let observer = NoopObserver;
        let approval =
            crate::approval::ApprovalManager::from_risk_profile(&RiskProfileConfig::default());
        let pacing = PacingConfig::default();
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: None,
            approval: Some(&approval),
            channel_name: "matrix",
            channel_reply_target: Some("room"),
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            turn_id: "turn-test",
            agent_alias: Some("agent"),
        };
        let marker = Arc::new(AtomicBool::new(true));
        let runtime_scope =
            control_plane::GoalRuntimeScope::new(Some(goal_ctx), None, Some(Arc::clone(&marker)));

        let outcome = control_plane::scope_goal_runtime(
            runtime_scope,
            gate_tool_approval(
                &ctx,
                "file_write",
                &serde_json::json!({"path": "/tmp/goal.txt", "content": "x"}),
                0,
            ),
        )
        .await;

        match outcome {
            ApprovalGateOutcome::Cancel => {}
            ApprovalGateOutcome::Proceed { .. }
            | ApprovalGateOutcome::Deny(_)
            | ApprovalGateOutcome::Replace(_) => {
                panic!("approval prompt in active goal must cancel the turn")
            }
        }
        let task = store.get(&task_id).await.unwrap().unwrap();
        assert_eq!(task.status, control_plane::TaskStatus::Paused);
        let goal = goal_store.get_goal_task(&task_id).await.unwrap().unwrap();
        assert_eq!(
            goal.pause_reason,
            Some(control_plane::GoalPauseReason::NeedsUserInput)
        );
        assert_eq!(goal.blockers.len(), 1);
        assert_eq!(
            goal.blockers[0].kind,
            control_plane::GoalBlockerKind::NeedsUserInput
        );
        assert_eq!(
            goal.blockers[0].message,
            i18n::get_required_cli_string_with_args(
                "goal-command-tool-approval-required-blocker",
                &[("tool", "file_write")]
            )
        );
        assert_eq!(
            goal.blockers[0]
                .payload
                .as_ref()
                .and_then(|payload| payload.get("tool"))
                .and_then(serde_json::Value::as_str),
            Some("file_write")
        );
    }
}
