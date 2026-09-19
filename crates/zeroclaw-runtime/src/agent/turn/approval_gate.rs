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
use zeroclaw_config::tool_policy::{Decision, Resolution, ResolutionReason};

pub(crate) enum ApprovalGateOutcome {
    Proceed {
        approved: bool,
        confirmation_id: Option<uuid::Uuid>,
        prompted: bool,
    },
    Deny {
        outcome: ToolExecutionOutcome,
        prompted: bool,
    },
    Replace {
        outcome: ToolExecutionOutcome,
        prompted: bool,
    },
    Cancelled,
}

impl ApprovalGateOutcome {
    pub(crate) fn prompted(&self) -> bool {
        match self {
            Self::Proceed { prompted, .. }
            | Self::Deny { prompted, .. }
            | Self::Replace { prompted, .. } => *prompted,
            Self::Cancelled => false,
        }
    }
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
    position: zeroclaw_api::channel::ApprovalPosition,
) -> ApprovalGateOutcome {
    let mut approval_requirement = ctx
        .approval
        .map(|mgr| mgr.approval_requirement(tool_name))
        .unwrap_or(ApprovalRequirement::NotRequired);
    let mut prompted = false;

    // ── RFC 7155 shell resolution ──────────────────────────────────
    // Only when the manager carries a policy context; everything else
    // (non-shell tools, configless paths) keeps the legacy tool-name flow.
    let mut shell_confirmation: Option<uuid::Uuid> = None;
    let mut shell_request_facts: Option<serde_json::Value> = None;
    if let Some(mgr) = ctx.approval
        && let Some(security) = mgr.policy()
        && tool_name == "shell"
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
                return ApprovalGateOutcome::Proceed {
                    approved: false,
                    confirmation_id: None,
                    prompted: false,
                };
            }
            Decision::Allow | Decision::Ask => {
                if (resolution.decision == Decision::Ask || mgr.hard_asks(tool_name))
                    && approval_requirement != ApprovalRequirement::Prompt
                {
                    if mgr.can_request_shell_approval() {
                        approval_requirement = ApprovalRequirement::Prompt;
                    } else {
                        let denied = crate::i18n::get_required_cli_string(
                            "tool-shell-approval-route-unavailable",
                        );
                        return ApprovalGateOutcome::Deny {
                            outcome: ToolExecutionOutcome {
                                output: denied.clone(),
                                success: false,
                                error_reason: Some(denied),
                                duration: Duration::ZERO,
                                receipt: None,
                                output_data: None,
                            },
                            prompted: false,
                        };
                    }
                }
                if approval_requirement == ApprovalRequirement::Prompt {
                    shell_request_facts = match mgr.shell_fingerprint_facts(command) {
                        Ok(facts) => Some(facts),
                        Err(error) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Reject
                                )
                                .with_category(::zeroclaw_log::EventCategory::Tool)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({"error": error.to_string()})),
                                "shell approval request facts could not be resolved"
                            );
                            let denied = crate::i18n::get_required_cli_string(
                                "tool-shell-execution-context-unverified",
                            );
                            return ApprovalGateOutcome::Deny {
                                outcome: ToolExecutionOutcome {
                                    output: denied.clone(),
                                    success: false,
                                    error_reason: Some(denied),
                                    duration: Duration::ZERO,
                                    receipt: None,
                                    output_data: None,
                                },
                                prompted: false,
                            };
                        }
                    };
                }
                // Prompt flow below; the Yes/Always branch mints the
                // confirmation.
            }
        }
    }

    if let Some(mgr) = ctx.approval
        && approval_requirement == ApprovalRequirement::Prompt
    {
        prompted = true;
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
                    position: Some(position),
                };
                let recipient = ctx.channel_reply_target.unwrap_or_default();
                let response = if let Some(cancel) = ctx.cancellation_token {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return ApprovalGateOutcome::Cancelled,
                        response = ch.request_approval_attributed(recipient, &ch_request) => response,
                    }
                } else {
                    ch.request_approval_attributed(recipient, &ch_request).await
                };
                match response {
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
        // confirmation is carried to the execution boundary. Nothing
        // model-supplied can produce one: the loop strips the injected bits
        // before this gate, and execution revalidates/finally consumes it.
        let mut confirmation_audit: Option<crate::approval::ConfirmationAudit> = None;
        if matches!(decision, ApprovalResponse::Yes | ApprovalResponse::Always)
            && let Some(command) = tool_args.get("command").and_then(serde_json::Value::as_str)
            && let Some(security) = mgr.policy()
            && tool_name == "shell"
        {
            let facts = match shell_request_facts.as_ref() {
                Some(facts) => facts,
                None => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_category(::zeroclaw_log::EventCategory::Tool)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(
                                ::serde_json::json!({"command_present": !command.is_empty()})
                            ),
                        "shell approval request facts were unavailable"
                    );
                    mgr.record_decision(tool_name, tool_args, &decision, &decision_channel, None);
                    let denied = crate::i18n::get_required_cli_string(
                        "tool-shell-execution-context-unverified",
                    );
                    return ApprovalGateOutcome::Deny {
                        outcome: ToolExecutionOutcome {
                            output: denied.clone(),
                            success: false,
                            error_reason: Some(denied),
                            duration: Duration::ZERO,
                            receipt: None,
                            output_data: None,
                        },
                        prompted: true,
                    };
                }
            };
            let confirmation = mgr.mint_confirmation(
                facts,
                zeroclaw_api::permission::RouteId::from(decision_channel.clone()),
                security.tool_policy.confirmation_validity_secs,
            );
            shell_confirmation = Some(confirmation.confirmation_id);
            confirmation_audit = Some(crate::approval::ConfirmationAudit {
                confirmation_id: confirmation.confirmation_id.to_string(),
                action_fingerprint: confirmation.action_fingerprint.as_hex(),
                trusted_route: confirmation.trusted_route.to_string(),
                terminal_state: "pending".to_string(),
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
            return ApprovalGateOutcome::Deny {
                outcome: ToolExecutionOutcome {
                    output: denied.clone(),
                    success: false,
                    error_reason: Some(denied),
                    duration: Duration::ZERO,
                    receipt: None,
                    output_data: None,
                },
                prompted: true,
            };
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
            return ApprovalGateOutcome::Replace {
                outcome: ToolExecutionOutcome {
                    output: crate::approval::sanitize_tool_replacement(replacement),
                    success: true,
                    error_reason: None,
                    duration: Duration::ZERO,
                    receipt: None,
                    output_data: None,
                },
                prompted: true,
            };
        }

        if matches!(decision, ApprovalResponse::Yes | ApprovalResponse::Always) {
            approval_requirement = ApprovalRequirement::Approved;
        }
    }

    ApprovalGateOutcome::Proceed {
        approved: shell_confirmation.is_some()
            || approval_requirement == ApprovalRequirement::Approved,
        confirmation_id: shell_confirmation,
        prompted,
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
        ResolutionReason::UnsafeProcessControlAssignment { .. } => {
            "process-control environment assignment is disallowed by policy"
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
    ApprovalGateOutcome::Deny {
        outcome: ToolExecutionOutcome {
            output: denied.clone(),
            success: false,
            error_reason: Some(denied),
            duration: Duration::ZERO,
            receipt: None,
            output_data: None,
        },
        prompted: false,
    }
}

#[cfg(test)]
mod tests {
    use super::{ApprovalGateOutcome, gate_tool_approval};
    use crate::agent::turn::context::TurnCtx;
    use crate::approval::{ApprovalManager, ApprovalResponse, ShellAuthorizationOutcome};
    use crate::observability::NoopObserver;
    use crate::platform::{NativeRuntime, RuntimeAdapter, ShellDialect};
    use crate::rpc::approval_channel::RpcApprovalChannel;
    use crate::rpc::context::ApprovalPendingMap;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use zeroclaw_api::jsonrpc::RpcOutbound;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig, StreamReasoningMode};
    use zeroclaw_config::tool_policy::{Decision, PolicyRuleConfig};

    fn full_auto_approve_profile_with_shell_ask() -> RiskProfileConfig {
        let mut profile = RiskProfileConfig {
            level: crate::security::AutonomyLevel::Full,
            auto_approve: vec!["shell".to_string()],
            always_ask: vec![" shell ".to_string()],
            allowed_commands: vec!["echo".to_string(), "true".to_string()],
            ..RiskProfileConfig::default()
        };
        profile.tool_policy.rules.push(PolicyRuleConfig {
            pattern: "Shell(echo:*)".to_string(),
            decision: Decision::Ask,
        });
        profile
    }

    #[tokio::test]
    async fn full_auto_approve_does_not_bypass_shell_ask_with_or_without_route() {
        let workspace = tempfile::TempDir::new().unwrap();
        let profile = full_auto_approve_profile_with_shell_ask();
        // Build the resolver without the manager's hard ask so this also
        // exercises the defensive Allow + hard-ask branch. Production
        // construction supplies the same canonicalized profile to both.
        let mut policy_profile = profile.clone();
        policy_profile.always_ask.clear();
        let security = Arc::new(crate::security::SecurityPolicy::from_risk_profile(
            &policy_profile,
            workspace.path(),
        ));
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::new());
        let shell =
            crate::tools::shell::ShellTool::new(Arc::clone(&security), Arc::clone(&runtime));
        let commands = ["echo ask", "printf unmatched", "true"];
        assert!(matches!(
            security
                .resolve_shell_decision(commands[1], ShellDialect::Posix, &[])
                .reason,
            zeroclaw_config::tool_policy::ResolutionReason::Unmatched
        ));
        assert_eq!(
            security
                .resolve_shell_decision(commands[2], ShellDialect::Posix, &[])
                .decision,
            Decision::Allow,
            "the hard always-ask case must cover an otherwise allowed command"
        );
        let observer = NoopObserver;
        let pacing = PacingConfig::default();

        let no_route = ApprovalManager::for_non_interactive(&profile);
        no_route.set_policy_context(Arc::clone(&security), ShellDialect::Posix);
        no_route.set_shell_execution_context(shell.execution_facts_resolver());
        let no_route_ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            temperature: None,
            approval: Some(&no_route),
            channel_name: "background",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "turn-no-route",
            agent_alias: Some("default"),
            parent_agent_alias: None,
            serving_provider_name: None,
            serving_model: None,
        };
        for command in commands {
            let arguments = serde_json::json!({"command": command});
            assert!(matches!(
                gate_tool_approval(
                    &no_route_ctx,
                    "shell",
                    &arguments,
                    0,
                    zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
                )
                .await,
                ApprovalGateOutcome::Deny { .. }
            ));
        }

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(4);
        let pending = Arc::new(ApprovalPendingMap::default());
        let channel = RpcApprovalChannel::new(
            "rpc",
            "session-ask",
            Arc::new(RpcOutbound::new(writer_tx)),
            Arc::clone(&pending),
            Default::default(),
        );
        let with_route = ApprovalManager::for_non_interactive_backchannel(&profile);
        with_route.set_policy_context(security, ShellDialect::Posix);
        with_route.set_shell_execution_context(shell.execution_facts_resolver());
        let route_ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            temperature: None,
            approval: Some(&with_route),
            channel_name: "rpc",
            channel_reply_target: Some("operator"),
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: Some(&channel),
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "turn-with-route",
            agent_alias: Some("default"),
            parent_agent_alias: None,
            serving_provider_name: None,
            serving_model: None,
        };
        for command in commands {
            let arguments = serde_json::json!({"command": command});
            let approval_wait = gate_tool_approval(
                &route_ctx,
                "shell",
                &arguments,
                0,
                zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
            );
            tokio::pin!(approval_wait);
            let line = tokio::select! {
                outcome = &mut approval_wait => panic!("Ask unexpectedly bypassed the approval route: {}", matches!(outcome, ApprovalGateOutcome::Proceed { .. })),
                line = writer_rx.recv() => line.expect("approval request notification"),
            };
            let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
            let request_id = frame["params"]["request_id"].as_str().unwrap();
            assert!(pending.resolve(
                request_id,
                zeroclaw_api::channel::ChannelApprovalResponse::Approve,
            ));
            assert!(matches!(
                approval_wait.await,
                ApprovalGateOutcome::Proceed {
                    approved: true,
                    confirmation_id: Some(_),
                    prompted: true,
                }
            ));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn posix_escape_cannot_bypass_shell_deny_at_gate_or_execution() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::TempDir::new().unwrap();
        let fake_git = workspace.path().join("git");
        let marker = workspace.path().join("escaped-deny-executed");
        std::fs::write(&fake_git, "#!/bin/sh\n: > \"$2\"\n").unwrap();
        std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut profile = RiskProfileConfig {
            level: crate::security::AutonomyLevel::Full,
            allowed_commands: vec!["git".to_string()],
            ..RiskProfileConfig::default()
        };
        profile.tool_policy.rules.push(PolicyRuleConfig {
            pattern: "Shell(git push:*)".to_string(),
            decision: Decision::Deny,
        });
        let security = Arc::new(crate::security::SecurityPolicy::from_risk_profile(
            &profile,
            workspace.path(),
        ));
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::new());
        let shell =
            crate::tools::shell::ShellTool::new(Arc::clone(&security), Arc::clone(&runtime));
        let approval = ApprovalManager::for_non_interactive(&profile);
        approval.set_policy_context(Arc::clone(&security), ShellDialect::Posix);
        approval.set_shell_execution_context(shell.execution_facts_resolver());

        let command = format!(
            "PATH={} git pu\\sh {}",
            workspace.path().display(),
            marker.display()
        );
        let arguments = serde_json::json!({"command": command});
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            temperature: None,
            approval: Some(&approval),
            channel_name: "background",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "turn-posix-escape-deny",
            agent_alias: Some("default"),
            parent_agent_alias: None,
            serving_provider_name: None,
            serving_model: None,
        };

        assert!(matches!(
            gate_tool_approval(
                &ctx,
                "shell",
                &arguments,
                0,
                zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
            )
            .await,
            ApprovalGateOutcome::Deny { .. }
        ));

        let result = shell.execute(arguments).await.unwrap();
        assert!(
            !result.success,
            "escaped deny unexpectedly executed: {result:?}"
        );
        assert!(
            !marker.exists(),
            "escaped command reached the fake git executable"
        );
    }

    #[tokio::test]
    async fn session_always_revalidates_policy_fingerprint_at_shell_boundary() {
        let workspace = tempfile::TempDir::new().unwrap();
        let profile = RiskProfileConfig::default();
        let mut security =
            crate::security::SecurityPolicy::from_risk_profile(&profile, workspace.path());
        security.allowed_commands.clear();
        let security = Arc::new(security);
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::new());
        let shell =
            crate::tools::shell::ShellTool::new(Arc::clone(&security), Arc::clone(&runtime));
        let approval = ApprovalManager::from_risk_profile(&profile);
        approval.set_policy_context(Arc::clone(&security), ShellDialect::Posix);
        approval.set_shell_execution_context(shell.execution_facts_resolver());
        let arguments = serde_json::json!({"command": "echo session-approved"});
        approval.record_decision("shell", &arguments, &ApprovalResponse::Always, "cli", None);

        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            temperature: None,
            approval: Some(&approval),
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "turn-session-always",
            agent_alias: Some("default"),
            parent_agent_alias: None,
            serving_provider_name: None,
            serving_model: None,
        };

        assert!(matches!(
            gate_tool_approval(
                &ctx,
                "shell",
                &arguments,
                0,
                zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
            )
            .await,
            ApprovalGateOutcome::Proceed {
                approved: false,
                confirmation_id: None,
                prompted: false,
            }
        ));
        let (authorization, fingerprint, _expires_at) =
            approval.authorize_shell_execution(&arguments);
        assert_eq!(authorization, ShellAuthorizationOutcome::SessionAllow);
        let result = shell
            .execute(serde_json::json!({
                "command": "echo session-approved",
                (crate::agent::RUNTIME_POLICY_ALLOW_ARG): true,
                (crate::agent::RUNTIME_CONFIRMATION_FINGERPRINT_ARG):
                    fingerprint.unwrap().as_hex(),
            }))
            .await
            .unwrap();
        assert!(
            result.success,
            "session-approved command failed: {result:?}"
        );
    }

    #[tokio::test]
    async fn cancelling_turn_drops_pending_channel_approval() {
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(4);
        let rpc = Arc::new(RpcOutbound::new(writer_tx));
        let pending = Arc::new(ApprovalPendingMap::default());
        let channel = RpcApprovalChannel::new(
            "rpc",
            "session-approval",
            rpc,
            Arc::clone(&pending),
            Default::default(),
        );
        let approval =
            ApprovalManager::for_non_interactive_backchannel(&RiskProfileConfig::default());
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let cancel = CancellationToken::new();
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            temperature: None,
            approval: Some(&approval),
            channel_name: "rpc",
            channel_reply_target: Some("operator"),
            cancellation_token: Some(&cancel),
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: Some(&channel),
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "turn-approval",
            agent_alias: Some("default"),
            parent_agent_alias: None,
            serving_provider_name: None,
            serving_model: None,
        };

        let arguments = serde_json::json!({"command": "sleep 60"});
        let approval_wait = gate_tool_approval(
            &ctx,
            "shell",
            &arguments,
            0,
            zeroclaw_api::channel::ApprovalPosition { index: 1, total: 1 },
        );
        tokio::pin!(approval_wait);
        let line = tokio::select! {
            outcome = &mut approval_wait => panic!("approval completed before cancellation: {}", matches!(outcome, ApprovalGateOutcome::Cancelled)),
            line = writer_rx.recv() => line.expect("approval request notification"),
        };
        let frame: serde_json::Value = serde_json::from_str(&line).expect("valid notification");
        let request_id = frame["params"]["request_id"]
            .as_str()
            .expect("approval request id")
            .to_string();
        assert!(pending.contains(&request_id));

        cancel.cancel();
        assert!(matches!(
            approval_wait.await,
            ApprovalGateOutcome::Cancelled
        ));
        assert!(
            !pending.contains(&request_id),
            "cancelling the turn must drop the stale approval responder"
        );
    }
}
