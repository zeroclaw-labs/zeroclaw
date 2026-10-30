//! Extracted per-call approval owner. Runtime bridges borrow transport and
//! Fluent renderers; this module owns gate ordering and lifecycle decisions.
use crate::approval::{ApprovalManager, ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use crate::tool_execution::ToolExecutionOutcome;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::channel::{ApprovalSource, Channel};

#[async_trait::async_trait]
pub trait ApprovalProgressPort: Send + Sync {
    async fn flush_narration(&self);
    async fn status(&self, status: String);
}

pub struct ApprovalContext<'a> {
    pub approval: Option<&'a ApprovalManager>,
    pub channel: Option<&'a dyn Channel>,
    pub channel_name: &'a str,
    pub channel_reply_target: Option<&'a str>,
    pub cancellation_token: Option<&'a CancellationToken>,
    pub progress: Option<&'a dyn ApprovalProgressPort>,
    pub model: &'a str,
    pub turn_id: &'a str,
    pub prompt_cli:
        &'a (dyn Fn(&ApprovalRequest) -> std::io::Result<ApprovalResponse> + Send + Sync),
    pub render_arguments_summary: &'a (dyn Fn(&serde_json::Value) -> String + Send + Sync),
    pub scrub_credentials: fn(&str) -> String,
}

pub enum ApprovalGateOutcome {
    Proceed { approved: bool },
    Deny(ToolExecutionOutcome),
    Replace(ToolExecutionOutcome),
    Cancelled,
    Error(anyhow::Error),
}

/// Run the approval flow for one tool call (upstream loop body, approval
/// section): resolve the tool's approval requirement, prompt interactively on
/// CLI or via the channel's inline approval on non-interactive channels
/// (falling back to auto-deny), and record the decision.
pub async fn gate_tool_approval(
    ctx: &ApprovalContext<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    iteration: usize,
    position: zeroclaw_api::channel::ApprovalPosition,
) -> ApprovalGateOutcome {
    let capture = zeroclaw_api::session_events::capture::current_tool_preparation();
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
        // This gate does not own an absolute response deadline. Channels keep
        // their actual timeout; interactive CLI input can remain unbounded.
        if let Some(port) = &capture
            && let Err(error) = port
                .request_approval(
                    zeroclaw_api::session_events::capture::ApprovalRequestIntent::default(),
                )
                .await
        {
            return ApprovalGateOutcome::Error(error);
        }

        // Interactive CLI: prompt the operator.
        // Non-interactive (channels): try the channel's inline
        // approval (e.g. Telegram inline keyboard) before falling
        // back to auto-deny.
        let (decision, decided_by, source, reply_event_id) = if mgr.is_non_interactive() {
            let attributed = if let Some(ch) = ctx.channel {
                let ch_request = zeroclaw_api::channel::ChannelApprovalRequest {
                    tool_name: request.tool_name.clone(),
                    arguments_summary: (ctx.render_arguments_summary)(&request.arguments),
                    raw_arguments: Some(request.arguments.clone()),
                    position: Some(position),
                };
                let recipient = ctx.channel_reply_target.unwrap_or_default();
                // Narration rides the async delta queue to the draft updater,
                // while this approval call goes to the channel directly and
                // can overtake it. A flush barrier waits until the updater
                // consumed (and flushed) the turn's narration, so the user
                // sees the pre-tool message before the approval prompt.
                if ch.supports_turn_flush_narration()
                    && let Some(tx) = ctx.progress
                {
                    tx.flush_narration().await;
                }
                let response = if let Some(cancel) = ctx.cancellation_token {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {
                            if let Some(port) = &capture
                                && let Err(error) = port.invalidate("approval_wait_cancelled").await {
                                    return ApprovalGateOutcome::Error(error);
                                }
                            return ApprovalGateOutcome::Cancelled;
                        },
                        response = ch.request_approval_attributed(recipient, &ch_request) => response,
                    }
                } else {
                    ch.request_approval_attributed(recipient, &ch_request).await
                };
                match response {
                    Ok(Some(a)) => Some(a),
                    Ok(None) if mgr.unsupported_backchannel_may_fall_back(tool_name) => {
                        // Preserve the historical shell-policy fallback when the
                        // channel has no approval prompt. Capture records the
                        // unsupported route explicitly; it does not turn the
                        // absence of a prompt into an approval or a denial.
                        if let Some(port) = &capture
                            && let Err(error) = port
                                .resolve_approval(
                                    zeroclaw_api::channel::ChannelApprovalResponse::Deny,
                                    "approval_channel_unsupported",
                                    None,
                                    serde_json::json!({
                                        "transport": "channel",
                                        "source": "unsupported_backchannel",
                                        "operator_reply_available": false,
                                    }),
                                )
                                .await
                        {
                            return ApprovalGateOutcome::Error(error);
                        }
                        return ApprovalGateOutcome::Proceed { approved: false };
                    }
                    Ok(None) => None,
                    Err(e) if zeroclaw_api::session_events::capture::is_capture_stopped(&e) => {
                        return ApprovalGateOutcome::Error(e);
                    }
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
                        Some(
                            zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                                zeroclaw_api::channel::ChannelApprovalResponse::Deny,
                                ApprovalSource::Unreachable,
                            ),
                        )
                    }
                }
            } else {
                None
            };
            // The deciding back-channel (when a fan-out bridge answered) rides
            // back on the response itself, so attribution can't be cross-wired
            // by a concurrent approval on the same channel instance.
            let decided_by = attributed.as_ref().and_then(|a| a.decided_by.clone());
            let reply_event_id = attributed.as_ref().and_then(|a| a.reply_event_id.clone());
            // Whether an operator actually decided, taken from the response's own
            // provenance rather than inferred.
            //
            // `attributed.is_none()` is NOT sufficient: a fail-closed approval route
            // returns `Some(Deny)` with no decider when the approver is missing,
            // unreachable, silent, or timed out, and a direct channel timeout does the
            // same. Those are runtime denials wearing an operator's clothes. Nor does
            // `decided_by.is_none()` work, since a single non-fan-out channel leaves
            // that `None` for a real human answer.
            let source = attributed
                .as_ref()
                .map(|a| a.source)
                .unwrap_or(ApprovalSource::Unavailable);
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
            (decision, decided_by, source, reply_event_id)
        } else {
            match (ctx.prompt_cli)(&request) {
                Ok(decision) => {
                    let reply_event_id = if let Some(port) = &capture {
                        let response = match &decision {
                            ApprovalResponse::Yes => {
                                zeroclaw_api::channel::ChannelApprovalResponse::Approve
                            }
                            ApprovalResponse::Always => {
                                zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove
                            }
                            ApprovalResponse::No => {
                                zeroclaw_api::channel::ChannelApprovalResponse::Deny
                            }
                            ApprovalResponse::ReplaceWith(replacement) => {
                                zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit {
                                    replacement: replacement.clone(),
                                }
                            }
                        };
                        match port.record_approval_reply(
                            zeroclaw_api::session_events::EventActor::Principal {
                                principal_id: zeroclaw_api::principal::PrincipalId::shared_operator().0,
                            },
                            response,
                            serde_json::json!({"transport":"cli","identity_class":"trusted_local_shared_operator"}),
                        ).await {
                            Ok(event_id) => Some(event_id),
                            Err(error) => return ApprovalGateOutcome::Error(error),
                        }
                    } else {
                        None
                    };
                    (decision, None, ApprovalSource::Operator, reply_event_id)
                }
                Err(_) => (
                    ApprovalResponse::No,
                    None,
                    ApprovalSource::Unreachable,
                    None,
                ),
            }
        };

        let unanswerable = source.is_runtime_fail_closed();
        let decision_channel = decided_by.unwrap_or_else(|| ctx.channel_name.to_string());
        if let Some(port) = &capture {
            let response = if unanswerable {
                zeroclaw_api::channel::ChannelApprovalResponse::Deny
            } else {
                match &decision {
                    ApprovalResponse::Yes => {
                        zeroclaw_api::channel::ChannelApprovalResponse::Approve
                    }
                    ApprovalResponse::Always => {
                        zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove
                    }
                    ApprovalResponse::No => zeroclaw_api::channel::ChannelApprovalResponse::Deny,
                    ApprovalResponse::ReplaceWith(replacement) => {
                        zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit {
                            replacement: replacement.clone(),
                        }
                    }
                }
            };
            let reason = if unanswerable {
                match source {
                    ApprovalSource::TimedOut => "approval_timed_out",
                    ApprovalSource::Unreachable => "approval_route_unreachable",
                    _ => "approval_route_unavailable",
                }
            } else if matches!(decision, ApprovalResponse::ReplaceWith(_)) {
                "operator_replaced_call"
            } else {
                "operator_denied_call"
            };
            if let Err(error) = port.resolve_approval(response, reason, reply_event_id, serde_json::json!({
                "channel":decision_channel,"operator_reply_available":!unanswerable,
                "source": match source {
                    ApprovalSource::Operator => "operator",
                    ApprovalSource::TimedOut => "timed_out",
                    ApprovalSource::Unreachable => "unreachable",
                    ApprovalSource::Unavailable => "unavailable",
                    _ => "runtime",
                },
                "decision":match decision {ApprovalResponse::Yes=>"approve_once",ApprovalResponse::Always=>"approve_session",ApprovalResponse::No=>"deny",ApprovalResponse::ReplaceWith(_)=>"replace"},
            })).await {
                return ApprovalGateOutcome::Error(error);
            }
        }
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
                // A real operator said no. The three-word form this replaces
                // carried the fact and none of its meaning, so the model
                // supplied the meaning itself and did not do it the same way
                // twice: on one run it reported the decline correctly, on the
                // next it offered three invented causes, none of them what
                // happened. The host owns the fact, so the host states what it
                // means. `Denied by user.` is kept as the opening sentence
                // because it is the phrase that distinguishes this path from
                // the runtime-generated denial above, and dropping it would
                // lose that distinction for every reader that already looks
                // for it.
                format!(
                    "Denied by user. The operator was asked to approve \
                     '{tool_name}' and declined, so the call did not run. Tell \
                     the user the request was declined. Do not retry this call \
                     and do not speculate about why it was declined."
                )
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
                        "arguments": (ctx.scrub_credentials)(&tool_args.to_string()),
                        "result": denied,
                        "trace_id": ctx.turn_id,
                        // Operator-facing only. The remedy lives here rather than
                        // in `result`, which is shown to the model: deciding to
                        // relax an approval policy is the operator's call, and
                        // putting the option in front of the model would invite it
                        // to lobby for its own privilege expansion.
                        "denied_by_runtime": unanswerable,
                        "operator_hint": if unanswerable {
                            if mgr.is_non_interactive() {
                                Some("No operator could be asked. Check that an approval-capable \
                                      channel is connected and that the agent's approval route names \
                                      a registered, reachable approver. If this tool should run \
                                      unattended, review the agent's risk profile deliberately.")
                            } else {
                                Some("CLI approval input was unavailable. Run with a controlling \
                                      terminal or readable stdin so an operator can answer. If this \
                                      tool should run unattended, review the agent's risk profile \
                                      deliberately.")
                            }
                        } else {
                            None
                        },
                    })),
                "tool_call_result"
            );
            if let Some(tx) = ctx.progress {
                tx.status(format!("\u{274c} {}: {}\n", tool_name, denied))
                    .await;
            }
            return ApprovalGateOutcome::Deny(ToolExecutionOutcome {
                output: denied.clone(),
                attachments: Vec::new(),
                success: false,
                error_reason: Some(denied),
                duration: Duration::ZERO,
                receipt: None,
                output_data: None,
            });
        }

        if let ApprovalResponse::ReplaceWith(replacement) = &decision {
            if let Some(tx) = ctx.progress {
                tx.status(format!("\u{270f} {}: replaced by user\n", tool_name))
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
                        "arguments": (ctx.scrub_credentials)(&tool_args.to_string()),
                        "replaced": true,
                        "output": (ctx.scrub_credentials)(replacement),
                        "trace_id": ctx.turn_id,
                    })),
                "tool_call_result"
            );
            return ApprovalGateOutcome::Replace(ToolExecutionOutcome {
                output: crate::approval::sanitize_tool_replacement(replacement),
                attachments: Vec::new(),
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
