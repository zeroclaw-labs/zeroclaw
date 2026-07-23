//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::approval::{ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use crate::control_plane::{
    GoalBlockerKind, GoalPauseState, apply_current_goal_approval_denial,
    current_goal_admission_context, current_goal_approval_deny_behavior,
    current_goal_turn_evaluation_requested, pause_current_goal_for_human_gate,
    resume_current_goal_after_human_gate,
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
    let mut approval_pause = None;
    if let Some(mgr) = ctx.approval
        && approval_requirement == ApprovalRequirement::Prompt
    {
        let request = ApprovalRequest {
            tool_name: tool_name.to_string(),
            arguments: tool_args.clone(),
        };
        // Goal tools stop at a durable human gate before the synchronous
        // request. A missing controller/store must fail closed rather than
        // allowing an approval response to race tool execution.
        if current_goal_turn_evaluation_requested() {
            let Some(pause) = pause_goal_for_tool_approval(ctx, &request).await else {
                return ApprovalGateOutcome::Cancel;
            };
            approval_pause = Some(pause);
        }
        // Interactive CLI: prompt the operator.
        // Non-interactive (channels): try the channel's inline
        // approval (e.g. Telegram inline keyboard) before falling
        // back to auto-deny.
        let (decision, decided_by, explicit_deny) = if mgr.is_non_interactive() {
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
            let (decision, explicit_deny) = match attributed.map(|a| a.response) {
                Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve) => {
                    (ApprovalResponse::Yes, false)
                }
                Some(zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove) => {
                    (ApprovalResponse::Always, false)
                }
                Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny) => {
                    (ApprovalResponse::No, true)
                }
                Some(zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit {
                    replacement,
                }) => (ApprovalResponse::ReplaceWith(replacement), false),
                // Channel doesn't support approval — auto-deny.
                None => (ApprovalResponse::No, false),
            };
            (decision, decided_by, explicit_deny)
        } else {
            let (decision, explicit_deny) = mgr.prompt_cli_with_provenance(&request);
            (decision, None, explicit_deny)
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
            let outcome = ToolExecutionOutcome {
                output: denied.clone(),
                success: false,
                error_reason: Some(denied),
                duration: Duration::ZERO,
                receipt: None,
                output_data: None,
            };
            if current_goal_turn_evaluation_requested() {
                if explicit_deny {
                    let behavior = current_goal_approval_deny_behavior();
                    if apply_explicit_goal_approval_denial(
                        ctx,
                        &request,
                        approval_pause.as_ref(),
                        behavior,
                    )
                    .await
                        && behavior == zeroclaw_config::schema::GoalApprovalDenyBehavior::Resume
                    {
                        return ApprovalGateOutcome::Deny(outcome);
                    }
                    return ApprovalGateOutcome::Cancel;
                }
                return ApprovalGateOutcome::Cancel;
            }
            return ApprovalGateOutcome::Deny(outcome);
        }

        if let ApprovalResponse::ReplaceWith(replacement) = &decision {
            if !resume_goal_after_approval_if_needed(approval_pause.as_ref()).await {
                return ApprovalGateOutcome::Cancel;
            }
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

    if !resume_goal_after_approval_if_needed(approval_pause.as_ref()).await {
        return ApprovalGateOutcome::Cancel;
    }

    ApprovalGateOutcome::Proceed {
        approved: approval_requirement == ApprovalRequirement::Approved,
    }
}

async fn resume_goal_after_approval_if_needed(expected_pause: Option<&GoalPauseState>) -> bool {
    let Some(expected_pause) = expected_pause else {
        return true;
    };
    let Some(goal_ctx) = current_goal_admission_context() else {
        return false;
    };
    matches!(
        resume_current_goal_after_human_gate(&goal_ctx, expected_pause.clone()).await,
        Ok(true)
    )
}

async fn apply_explicit_goal_approval_denial(
    _ctx: &TurnCtx<'_>,
    request: &ApprovalRequest,
    expected_pause: Option<&GoalPauseState>,
    behavior: zeroclaw_config::schema::GoalApprovalDenyBehavior,
) -> bool {
    let Some(goal_ctx) = current_goal_admission_context() else {
        return false;
    };
    let message = crate::i18n::get_required_cli_string_with_args(
        "goal-command-tool-approval-required-blocker",
        &[("tool", &request.tool_name)],
    );
    let payload = serde_json::json!({
        "tool": request.tool_name.as_str(),
        "arguments_summary": crate::approval::summarize_args(&request.arguments),
    });
    match apply_current_goal_approval_denial(
        &goal_ctx,
        behavior,
        GoalBlockerKind::NeedsUserInput,
        message,
        Some(payload),
        match expected_pause {
            Some(pause) => pause.clone(),
            None => return false,
        },
    )
    .await
    {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{error:#}")})),
                "goal approval denial transition failed"
            );
            false
        }
    }
}

async fn pause_goal_for_tool_approval(
    ctx: &TurnCtx<'_>,
    request: &ApprovalRequest,
) -> Option<GoalPauseState> {
    if !current_goal_turn_evaluation_requested() {
        return None;
    }
    let goal_ctx = current_goal_admission_context()?;
    let arguments_summary = crate::approval::summarize_args(&request.arguments);
    let message = crate::i18n::get_required_cli_string_with_args(
        "goal-command-tool-approval-required-blocker",
        &[("tool", &request.tool_name)],
    );
    let payload = serde_json::json!({
        "tool": request.tool_name.as_str(),
        "arguments_summary": arguments_summary,
    });
    let expected_pause = GoalPauseState {
        reason: crate::control_plane::GoalPauseReason::NeedsUserInput,
        description: Some(message.clone()),
        blockers: vec![crate::control_plane::GoalBlocker {
            kind: GoalBlockerKind::NeedsUserInput,
            message: message.clone(),
            payload: Some(payload.clone()),
        }],
    };
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
            Some(expected_pause)
        }
        Ok(None) => None,
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
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::noop::NoopObserver;
    use crate::{control_plane, i18n};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig};

    struct FixedApprovalChannel {
        response: Option<zeroclaw_api::channel::ChannelApprovalResponse>,
        requests: Arc<AtomicUsize>,
        pause_observer: Option<(
            Arc<dyn control_plane::TaskRegistry>,
            String,
            Arc<AtomicBool>,
        )>,
        /// Test-only race injector: changes the canonical task state after the
        /// durable pre-prompt pause and before the gate attempts its resume.
        resume_race: Option<(Arc<dyn control_plane::GoalTaskRegistry>, String)>,
    }

    impl zeroclaw_api::attribution::Attributable for FixedApprovalChannel {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Channel(
                zeroclaw_api::attribution::ChannelKind::AcpChannel,
            )
        }

        fn alias(&self) -> &str {
            "approval-test"
        }
    }

    #[async_trait::async_trait]
    impl zeroclaw_api::channel::Channel for FixedApprovalChannel {
        fn name(&self) -> &str {
            "approval-test"
        }

        async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn request_approval(
            &self,
            _recipient: &str,
            _request: &zeroclaw_api::channel::ChannelApprovalRequest,
        ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            if let Some((store, task_id, observed_paused)) = &self.pause_observer
                && let Ok(Some(task)) = store.get(task_id).await
            {
                observed_paused.store(
                    task.status == control_plane::TaskStatus::Paused,
                    Ordering::SeqCst,
                );
            }
            if let Some((store, task_id)) = &self.resume_race {
                let _ = store
                    .pause_goal_task_if_status(
                        task_id,
                        control_plane::TaskStatus::Paused,
                        control_plane::GoalPauseState {
                            reason: control_plane::GoalPauseReason::OperatorPaused,
                            description: Some("later operator pause".into()),
                            blockers: vec![control_plane::GoalBlocker {
                                kind: control_plane::GoalBlockerKind::NeedsUserInput,
                                message: "later blocker".into(),
                                payload: None,
                            }],
                        },
                    )
                    .await;
            }
            Ok(self.response.clone())
        }
    }

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
    async fn missing_goal_approval_route_keeps_goal_paused_and_cancels_turn() {
        let (store, goal_store, task_id, goal_ctx) = create_running_goal().await;
        let observer = NoopObserver;
        let approval =
            crate::approval::ApprovalManager::for_non_interactive(&RiskProfileConfig::default());
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
            parent_agent_alias: None,
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
                panic!("unavailable goal approval route must cancel the turn")
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

    async fn goal_approval_outcome(
        behavior: zeroclaw_config::schema::GoalApprovalDenyBehavior,
        response: Option<zeroclaw_api::channel::ChannelApprovalResponse>,
    ) -> (ApprovalGateOutcome, control_plane::TaskStatus, usize, bool) {
        goal_approval_outcome_with_resume_race(behavior, response, false).await
    }

    async fn goal_approval_outcome_with_resume_race(
        behavior: zeroclaw_config::schema::GoalApprovalDenyBehavior,
        response: Option<zeroclaw_api::channel::ChannelApprovalResponse>,
        cancel_before_resume: bool,
    ) -> (ApprovalGateOutcome, control_plane::TaskStatus, usize, bool) {
        let (store, goal_store, task_id, goal_ctx) = create_running_goal().await;
        let observer = NoopObserver;
        let approval = crate::approval::ApprovalManager::for_non_interactive(&RiskProfileConfig {
            always_ask: vec!["file_write".into()],
            ..RiskProfileConfig::default()
        });
        let pacing = PacingConfig::default();
        let requests = Arc::new(AtomicUsize::new(0));
        let observed_paused = Arc::new(AtomicBool::new(false));
        let channel = FixedApprovalChannel {
            response,
            requests: Arc::clone(&requests),
            pause_observer: Some((
                Arc::clone(&store),
                task_id.clone(),
                Arc::clone(&observed_paused),
            )),
            resume_race: cancel_before_resume.then(|| (Arc::clone(&goal_store), task_id.clone())),
        };
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: None,
            approval: Some(&approval),
            channel_name: "approval-test",
            channel_reply_target: Some("durable-route"),
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: Some(&channel),
            turn_id: "turn-test",
            agent_alias: Some("agent"),
            parent_agent_alias: None,
        };
        let marker = Arc::new(AtomicBool::new(true));
        let runtime_scope =
            control_plane::GoalRuntimeScope::new(Some(goal_ctx), None, Some(Arc::clone(&marker)))
                .with_approval_deny_behavior_resolver(Arc::new(move || behavior));
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
        let status = store.get(&task_id).await.unwrap().unwrap().status;
        (
            outcome,
            status,
            requests.load(Ordering::SeqCst),
            observed_paused.load(Ordering::SeqCst),
        )
    }

    #[tokio::test]
    async fn explicit_goal_denial_pause_keeps_the_exact_goal_paused_and_stops_the_turn() {
        let (outcome, status, requests, _) = goal_approval_outcome(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Pause,
            Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny),
        )
        .await;

        assert_eq!(requests, 1);
        assert_eq!(status, control_plane::TaskStatus::Paused);
        assert!(matches!(outcome, ApprovalGateOutcome::Cancel));
    }

    #[tokio::test]
    async fn explicit_goal_denial_cancel_cancels_the_exact_goal_and_stops_the_turn() {
        let (outcome, status, requests, _) = goal_approval_outcome(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Cancel,
            Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny),
        )
        .await;

        assert_eq!(requests, 1);
        assert_eq!(status, control_plane::TaskStatus::Cancelled);
        assert!(matches!(outcome, ApprovalGateOutcome::Cancel));
    }

    #[tokio::test]
    async fn explicit_goal_denial_resume_restores_running_and_returns_one_failed_result() {
        let (outcome, status, requests, _) = goal_approval_outcome(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Resume,
            Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny),
        )
        .await;

        assert_eq!(requests, 1);
        assert_eq!(status, control_plane::TaskStatus::Running);
        let ApprovalGateOutcome::Deny(result) = outcome else {
            panic!("resume policy must return the ordinary failed tool result to the loop");
        };
        assert!(!result.success);
        assert!(
            result.receipt.is_none(),
            "denial must not synthesize a duplicate receipt"
        );
    }

    #[tokio::test]
    async fn missing_goal_approval_response_keeps_the_pre_prompt_pause_and_cancels() {
        let (outcome, status, requests, _) = goal_approval_outcome(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Resume,
            None,
        )
        .await;

        assert_eq!(requests, 1);
        assert_eq!(status, control_plane::TaskStatus::Paused);
        assert!(matches!(outcome, ApprovalGateOutcome::Cancel));
    }

    #[tokio::test]
    async fn goal_approval_pauses_before_prompt_and_restores_running_before_proceeding() {
        let (outcome, status, requests, observed_paused) = goal_approval_outcome(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Pause,
            Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve),
        )
        .await;

        assert_eq!(requests, 1);
        assert!(
            observed_paused,
            "the exact task must be durably paused before the prompt"
        );
        assert_eq!(status, control_plane::TaskStatus::Running);
        assert!(matches!(
            outcome,
            ApprovalGateOutcome::Proceed { approved: true }
        ));
    }

    #[tokio::test]
    async fn goal_approval_edit_resumes_before_replacing_the_unexecuted_tool() {
        let (outcome, status, requests, observed_paused) = goal_approval_outcome(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Pause,
            Some(
                zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit {
                    replacement: "operator supplied replacement".into(),
                },
            ),
        )
        .await;

        assert_eq!(requests, 1);
        assert!(observed_paused);
        assert_eq!(status, control_plane::TaskStatus::Running);
        let ApprovalGateOutcome::Replace(result) = outcome else {
            panic!("the loop must receive exactly one replacement instead of executing the tool");
        };
        assert_eq!(result.output, "operator supplied replacement");
        assert!(result.success);
        assert!(result.receipt.is_none());
    }

    #[tokio::test]
    async fn goal_approval_edit_cancels_when_exact_resume_loses_the_lifecycle_race() {
        let (outcome, status, requests, observed_paused) = goal_approval_outcome_with_resume_race(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Pause,
            Some(
                zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit {
                    replacement: "must not reach the loop".into(),
                },
            ),
            true,
        )
        .await;

        assert_eq!(requests, 1);
        assert!(observed_paused);
        assert_eq!(status, control_plane::TaskStatus::Paused);
        assert!(matches!(outcome, ApprovalGateOutcome::Cancel));
    }

    #[tokio::test]
    async fn goal_approval_success_cancels_when_later_operator_pause_wins() {
        let (outcome, status, requests, observed_paused) = goal_approval_outcome_with_resume_race(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Pause,
            Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve),
            true,
        )
        .await;
        assert_eq!(requests, 1);
        assert!(observed_paused);
        assert_eq!(status, control_plane::TaskStatus::Paused);
        assert!(matches!(outcome, ApprovalGateOutcome::Cancel));
    }

    #[tokio::test]
    async fn explicit_goal_denial_resume_cancels_when_later_operator_pause_wins() {
        let (outcome, status, requests, observed_paused) = goal_approval_outcome_with_resume_race(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Resume,
            Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny),
            true,
        )
        .await;
        assert_eq!(requests, 1);
        assert!(observed_paused);
        assert_eq!(status, control_plane::TaskStatus::Paused);
        assert!(matches!(outcome, ApprovalGateOutcome::Cancel));
    }

    #[tokio::test]
    async fn explicit_goal_denial_cancel_keeps_later_operator_pause() {
        let (outcome, status, requests, observed_paused) = goal_approval_outcome_with_resume_race(
            zeroclaw_config::schema::GoalApprovalDenyBehavior::Cancel,
            Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny),
            true,
        )
        .await;
        assert_eq!(requests, 1);
        assert!(observed_paused);
        assert_eq!(status, control_plane::TaskStatus::Paused);
        assert!(matches!(outcome, ApprovalGateOutcome::Cancel));
    }
}
