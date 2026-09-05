//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording. For shell-family tools under an
//! attached policy context, the gate first RESOLVES the actual command
//! (RFC 7155): a hard `Deny` never reaches a prompt, an `Allow`-tier
//! resolution skips the prompt, and only an `Ask` tier prompts — with the
//! approval minting a fingerprint-bound confirmation.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::approval::{ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use std::time::Duration;
use zeroclaw_api::permission::ConsumeOutcome;
use zeroclaw_config::tool_policy::{Decision, Resolution, ResolutionReason};

pub(crate) enum ApprovalGateOutcome {
    Proceed { approved: bool },
    Deny(ToolExecutionOutcome),
    Replace(ToolExecutionOutcome),
}

/// Run the approval flow for one tool call (upstream loop body, approval
/// section): resolve the tool's approval requirement, prompt interactively on
/// CLI or via the channel's inline approval on non-interactive channels
/// (falling back to auto-deny), and record the decision.
///
/// For shell-family tools whose manager carries a policy context, the
/// command is resolved through the rule table first (RFC 7155 §3.2/§8):
/// `Deny` → a synthesized denial without a prompt; `Allow` (without a hard
/// `always_ask` on the tool) → proceed without a prompt — no confirmation is
/// needed for an explicitly-allowed command; `Ask` → the prompt flow, where
/// an approval mints a single-use confirmation bound to the command's
/// action fingerprint, and the returned `approved` bit means "a
/// confirmation was consumed" (never "the tool-name layer said yes").
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

    // ── RFC 7155 shell resolution ──────────────────────────────────
    // Only when the manager carries a policy context; everything else
    // (non-shell tools, configless paths) keeps the legacy tool-name flow.
    let mut shell_confirmation: Option<bool> = None;
    if let Some(mgr) = ctx.approval
        && let Some(security) = mgr.policy()
        && crate::agent::is_runtime_approved_arg_tool(tool_name)
        && let Some(command) = tool_args.get("command").and_then(serde_json::Value::as_str)
    {
        let resolution =
            security.resolve_shell_decision(command, mgr.shell_dialect(), &mgr.session_rules());
        match resolution.decision {
            Decision::Deny => {
                return shell_denied_outcome(ctx, tool_name, tool_args, iteration, &resolution)
                    .await;
            }
            Decision::Allow if !mgr.hard_asks(tool_name) => {
                // Allow tier: explicitly allowed, no approval needed.
                return ApprovalGateOutcome::Proceed { approved: false };
            }
            Decision::Allow | Decision::Ask => {
                if approval_requirement != ApprovalRequirement::Prompt {
                    // An Ask-tier command on a route that cannot ask
                    // (full autonomy, auto_approve, or a non-interactive
                    // manager without a back-channel): proceed unapproved —
                    // the shell tool's confirmed validation then fails
                    // closed. Tool-level approval can never bypass a
                    // command-level Ask (RFC 7155 §1.3).
                    shell_confirmation = Some(false);
                }
                // Prompt flow below; the Yes/Always branch mints the
                // confirmation.
            }
        }
    }

    if let Some(mgr) = ctx.approval
        && approval_requirement == ApprovalRequirement::Prompt
    {
        let request = ApprovalRequest {
            tool_name: tool_name.to_string(),
            arguments: tool_args.clone(),
            // RFC 7155 §5.5: display-only, untrusted, clamped.
            intent: tool_args
                .get("intent")
                .and_then(serde_json::Value::as_str)
                .map(|intent| intent.chars().take(200).collect::<String>()),
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

        let decision_channel = decided_by
            .clone()
            .unwrap_or_else(|| ctx.channel_name.to_string());

        // RFC 7155 §5.1/§5.2: for the resolved shell command, the
        // operator's approval mints a single-use confirmation bound to the
        // command's action fingerprint, and `approved` means the
        // confirmation was consumed. Nothing model-supplied can produce
        // one: the loop strips the injected bits before this gate.
        // `shell_confirmation` is the OUTER one from the resolution
        // pre-check; the consumed result must reach the final Proceed.
        let mut confirmation_audit: Option<crate::approval::ConfirmationAudit> = None;
        if matches!(decision, ApprovalResponse::Yes | ApprovalResponse::Always)
            && let Some(command) = tool_args.get("command").and_then(serde_json::Value::as_str)
            && let Some(security) = mgr.policy()
            && crate::agent::is_runtime_approved_arg_tool(tool_name)
        {
            let dialect = mgr.shell_dialect();
            let action = zeroclaw_config::tool_policy::extract_shell_action(
                command,
                dialect,
                Some(&security.workspace_dir),
            );
            let zeroclaw_config::tool_policy::ToolAction::Shell(shell_action) = &action;
            let facts = shell_action.fingerprint_facts();
            let confirmation = mgr.mint_confirmation(
                &facts,
                zeroclaw_api::permission::RouteId::from(decision_channel.clone()),
                security.tool_policy.confirmation_validity_secs,
            );
            let outcome = mgr.consume_confirmation(&confirmation.confirmation_id, &facts);
            if outcome != ConsumeOutcome::Consumed {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "tool": tool_name,
                            "consume_outcome": format!("{outcome:?}"),
                            "trace_id": ctx.turn_id,
                        })),
                    "confirmation consume failed right after mint"
                );
            }
            shell_confirmation = Some(outcome == ConsumeOutcome::Consumed);
            confirmation_audit = Some(crate::approval::ConfirmationAudit {
                action_fingerprint: confirmation.action_fingerprint.as_hex(),
                trusted_route: confirmation.trusted_route.to_string(),
                terminal_state: format!("{outcome:?}").to_lowercase(),
            });
        }
        mgr.record_decision(
            tool_name,
            tool_args,
            &decision,
            &decision_channel,
            confirmation_audit,
        );

        if decision == ApprovalResponse::No {
            // This string is fed back to the MODEL, so it states the outcome and
            // stops there. It deliberately does not name the settings that would
            // permit the call: `auto_approve` bypasses operator approval for that
            // tool and `level = "full"` removes approval gates for every tool and
            // drops workspace-only confinement. Putting that remedy in front of
            // the model invites it to argue for expanding its own privileges,
            // which is a disproportionate response to an approval channel being
            // unavailable. Operators get the actionable advice through the WARN
            // record below and the UI, where changing policy is actually their
            // decision to make.
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
        approved: shell_confirmation
            .unwrap_or(approval_requirement == ApprovalRequirement::Approved),
    }
}

/// The synthesized denial for a resolver-`Deny` shell command: no prompt
/// happens, because no approval could change the outcome (RFC 7155 §1.3:
/// no `Allow` from any source can overturn a matched `Deny`).
async fn shell_denied_outcome(
    ctx: &TurnCtx<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    iteration: usize,
    resolution: &Resolution,
) -> ApprovalGateOutcome {
    let reason = match resolution.reason {
        ResolutionReason::HighRiskBlocked => "high-risk command is disallowed by policy",
        ResolutionReason::NoShellAccess => "configured runtime has no shell access",
        ResolutionReason::DegradedSyntax { .. } => {
            "command syntax cannot be safely evaluated under this policy"
        }
        _ => "command is not allowed by security policy",
    };
    let denied = format!(
        "Tool call not executed: {reason}. No approval can change this outcome; \
         it is fixed by the security policy."
    );
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
                "denied_by_policy": true,
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
    ApprovalGateOutcome::Deny(ToolExecutionOutcome {
        output: denied.clone(),
        success: false,
        error_reason: Some(denied),
        duration: Duration::ZERO,
        receipt: None,
        output_data: None,
    })
}
