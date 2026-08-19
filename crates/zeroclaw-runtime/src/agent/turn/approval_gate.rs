//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::approval::{ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use std::time::Duration;

pub(crate) enum ApprovalGateOutcome {
    Proceed { approved: bool },
    Deny(ToolExecutionOutcome),
    Replace(ToolExecutionOutcome),
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

        // Interactive CLI: prompt the operator.
        // Non-interactive (channels): try the channel's inline
        // approval (e.g. Telegram inline keyboard) before falling
        // back to auto-deny.
        let (decision, decided_by, unanswerable) = if mgr.is_non_interactive() {
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
            // Whether an operator actually decided, taken from the response's own
            // provenance rather than inferred.
            //
            // `attributed.is_none()` is NOT sufficient: a fail-closed approval route
            // returns `Some(Deny)` with no decider when the approver is missing,
            // unreachable, silent, or timed out, and a direct channel timeout does the
            // same. Those are runtime denials wearing an operator's clothes. Nor does
            // `decided_by.is_none()` work, since a single non-fan-out channel leaves
            // that `None` for a real human answer.
            let unanswerable = attributed
                .as_ref()
                .map(|a| a.source.is_runtime_fail_closed())
                .unwrap_or(true);
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
            (decision, decided_by, unanswerable)
        } else {
            (mgr.prompt_cli(&request), None, false)
        };

        let decision_channel = decided_by.unwrap_or_else(|| ctx.channel_name.to_string());
        mgr.record_decision(tool_name, tool_args, &decision, &decision_channel);

        if decision == ApprovalResponse::No {
            // This string is fed back to the MODEL, so it states the outcome and
            // stops there. It deliberately does not name the settings that would
            // permit the call: `auto_approve` bypasses operator approval for that
            // tool and `level = "full"` auto-approves uncovered tools (tools in
            // `always_ask` still prompt or fail closed). Putting that remedy in
            // front of the model invites it to argue for expanding its own
            // privileges, which is a disproportionate response to an approval
            // channel being unavailable.
            // Operators get the actionable advice through the WARN record below and
            // the UI, where changing policy is actually their decision to make.
            let denied = if unanswerable {
                format!(
                    "Tool call not executed: '{tool_name}' requires approval and no operator \
                     decision was available, so the runtime denied it by policy. This was not \
                     a user's decision."
                )
            } else {
                "Denied by user.".to_string()
            };
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
                        // Operator-facing only. The remedy lives here rather than
                        // in `result`, which is shown to the model: deciding to
                        // relax an approval policy is the operator's call, and
                        // putting the option in front of the model would invite it
                        // to lobby for its own privilege expansion.
                        "denied_by_runtime": unanswerable,
                        "operator_hint": if unanswerable {
                            Some("No operator could be asked. Check that an approval-capable \
                                  channel is connected and that the agent's approval route names \
                                  a registered, reachable approver. If this tool should run \
                                  unattended, review the agent's risk profile deliberately.")
                        } else {
                            None
                        },
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

#[cfg(test)]
mod tests {
    use super::super::context::TurnCtx;
    use super::{ApprovalGateOutcome, gate_tool_approval};
    use crate::approval::ApprovalManager;
    use crate::observability::NoopObserver;
    use crate::security::AutonomyLevel;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
    use zeroclaw_api::channel::{
        Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
    };
    use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig};

    fn full_always_ask_profile() -> RiskProfileConfig {
        RiskProfileConfig {
            level: AutonomyLevel::Full,
            always_ask: vec!["shell".into()],
            ..RiskProfileConfig::default()
        }
    }

    fn test_ctx<'a>(
        observer: &'a NoopObserver,
        pacing: &'a PacingConfig,
        approval: Option<&'a ApprovalManager>,
        channel: Option<&'a dyn Channel>,
    ) -> TurnCtx<'a> {
        TurnCtx {
            parent_agent_alias: None,
            observer,
            provider_name: "stub",
            model: "stub-model",
            temperature: None,
            approval,
            channel_name: "test",
            channel_reply_target: Some("operator"),
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing,
            strict_tool_parsing: false,
            channel,
            agent_alias: None,
            turn_id: "trace-approval-gate",
        }
    }

    struct ApprovingChannel {
        approval_requests: Arc<AtomicUsize>,
    }

    impl Attributable for ApprovingChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::AcpChannel)
        }
        fn alias(&self) -> &str {
            "approving-test"
        }
    }

    #[async_trait]
    impl Channel for ApprovingChannel {
        fn name(&self) -> &str {
            "approving-test"
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn request_approval(
            &self,
            _recipient: &str,
            _request: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            self.approval_requests.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ChannelApprovalResponse::Approve))
        }
    }

    #[tokio::test]
    async fn full_always_ask_fail_closed_without_channel_does_not_execute() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let profile = full_always_ask_profile();
        let approval = ApprovalManager::for_non_interactive(&profile);
        let ctx = test_ctx(&observer, &pacing, Some(&approval), None);

        match gate_tool_approval(&ctx, "shell", &serde_json::json!({"command": "ls"}), 0).await {
            ApprovalGateOutcome::Deny(outcome) => {
                assert!(!outcome.success);
                assert!(
                    outcome.output.contains("requires approval"),
                    "plain non-interactive Full+always_ask must fail closed, got {}",
                    outcome.output
                );
            }
            ApprovalGateOutcome::Proceed { approved } => {
                panic!("listed Full tool must not silently execute (approved={approved})")
            }
            ApprovalGateOutcome::Replace(_) => panic!("listed Full tool must not be replaced"),
        }

        match gate_tool_approval(&ctx, "file_write", &serde_json::json!({"path": "x"}), 0).await {
            ApprovalGateOutcome::Proceed { approved: true } => {}
            ApprovalGateOutcome::Proceed { approved: false } => {
                panic!("uncovered Full tool must still auto-approve")
            }
            ApprovalGateOutcome::Deny(_) | ApprovalGateOutcome::Replace(_) => {
                panic!("uncovered Full tool must still auto-approve")
            }
        }
    }

    #[tokio::test]
    async fn full_always_ask_backchannel_requests_approval_and_uncovered_still_executes() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let profile = full_always_ask_profile();
        let approval = ApprovalManager::for_non_interactive_backchannel(&profile);
        let requests = Arc::new(AtomicUsize::new(0));
        let channel = ApprovingChannel {
            approval_requests: Arc::clone(&requests),
        };
        let ctx = test_ctx(&observer, &pacing, Some(&approval), Some(&channel));

        match gate_tool_approval(&ctx, "shell", &serde_json::json!({"command": "ls"}), 0).await {
            ApprovalGateOutcome::Proceed { approved: true } => {}
            ApprovalGateOutcome::Proceed { approved: false } => {
                panic!("back-channel approval must mark the listed tool approved")
            }
            ApprovalGateOutcome::Deny(outcome) => {
                panic!(
                    "back-channel approval must permit the listed tool, got {}",
                    outcome.output
                )
            }
            ApprovalGateOutcome::Replace(_) => panic!("unexpected replace"),
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "listed Full tool must go through the real back-channel request path"
        );

        match gate_tool_approval(&ctx, "file_write", &serde_json::json!({"path": "x"}), 0).await {
            ApprovalGateOutcome::Proceed { approved: true } => {}
            ApprovalGateOutcome::Proceed { approved: false }
            | ApprovalGateOutcome::Deny(_)
            | ApprovalGateOutcome::Replace(_) => {
                panic!("uncovered Full tool must still auto-approve")
            }
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "uncovered Full tool must not prompt the back-channel"
        );
    }
}
