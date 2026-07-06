//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::approval::{ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use std::time::Duration;

/// Outcome of [`gate_tool_approval`] for one tool call.
///
/// `Deny`/`Replace` carry the synthesized [`ToolExecutionOutcome`] the caller
/// records into its `ordered_results` slot before skipping execution;
/// `Proceed::approved` feeds `set_runtime_approved_arg`.
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
        let decision = if mgr.is_non_interactive() {
            let channel_decision = if let Some(ch) = ctx.channel {
                let ch_request = zeroclaw_api::channel::ChannelApprovalRequest {
                    tool_name: request.tool_name.clone(),
                    arguments_summary: crate::approval::summarize_args(&request.arguments),
                    raw_arguments: Some(request.arguments.clone()),
                };
                let recipient = ctx.channel_reply_target.unwrap_or_default();
                match ch.request_approval(recipient, &ch_request).await {
                    Ok(Some(r)) => Some(r),
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
            match channel_decision {
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
            }
        } else {
            mgr.prompt_cli(&request)
        };

        // The approval audit records which surface decided. On the streaming
        // path `ctx.channel` is the approval bridge fanning out to several
        // registered back-channels, and `ctx.channel_name` is the loop's
        // static "cli"; prefer the back-channel that actually answered so a
        // WS/ACP approval is attributed to WS/ACP, not "cli". Single channels
        // and the CLI prompt path report `None` and keep `channel_name`.
        let decision_channel = ctx
            .channel
            .and_then(|ch| ch.last_decision_channel())
            .unwrap_or_else(|| ctx.channel_name.to_string());
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
