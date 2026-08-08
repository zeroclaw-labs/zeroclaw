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
            // Host-computed from the arguments' effects by the tool itself;
            // the model cannot author what the operator reads here.
            host_summary: ctx
                .tool_by_name(tool_name)
                .and_then(|tool| tool.approval_summary(tool_args)),
        };

        // Interactive CLI: prompt the operator.
        // Non-interactive (channels): try the channel's inline
        // approval (e.g. Telegram inline keyboard) before falling
        // back to auto-deny.
        let mut operator_only_channel_refused = false;
        let (decision, decided_by, unanswerable) = if mgr.is_non_interactive() {
            let operator_only = ctx
                .tool_by_name(tool_name)
                .is_some_and(|tool| tool.approval_requires_operator());
            let attributed = if let Some(ch) = ctx.channel {
                if operator_only && !ch.is_operator_approval_surface() {
                    // Whoever holds this chat account is not necessarily the
                    // operator; for a tool that grants authority, the channel
                    // is told what's pending but never handed the decision.
                    operator_only_channel_refused = true;
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_category(::zeroclaw_log::EventCategory::Tool)
                            .with_attrs(::serde_json::json!({
                                "tool": tool_name,
                                "channel": ctx.channel_name,
                                "trace_id": ctx.turn_id,
                            })),
                        "operator-only tool approval not offered to a chat channel"
                    );
                    None
                } else {
                    let ch_request = zeroclaw_api::channel::ChannelApprovalRequest {
                        tool_name: request.tool_name.clone(),
                        arguments_summary: request
                            .host_summary
                            .clone()
                            .unwrap_or_else(|| crate::approval::summarize_args(&request.arguments)),
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
            // tool and `level = "full"` removes approval gates for every tool and
            // drops workspace-only confinement. Putting that remedy in front of the
            // model invites it to argue for expanding its own privileges, which is a
            // disproportionate response to an approval channel being unavailable.
            // Operators get the actionable advice through the WARN record below and
            // the UI, where changing policy is actually their decision to make.
            //
            // `operator_only_channel_refused` is checked first: it is the more
            // specific cause (we deliberately did not ask a chat channel that
            // could otherwise have answered), and it always coincides with
            // `unanswerable` since no decision was collected.
            let denied = if operator_only_channel_refused {
                format!(
                    "`{tool_name}` requires operator approval and cannot be approved \
                     from a chat channel. Not approved. The operator can run this \
                     from the terminal or a paired client."
                )
            } else if unanswerable {
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
    use super::*;
    use crate::approval::ApprovalManager;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role, ToolKind};
    use zeroclaw_api::channel::{
        Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
    };
    use zeroclaw_api::tool::{Tool, ToolResult};
    use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig};

    /// A channel that always answers Approve and counts how often it is asked.
    struct StubChannel {
        asked: AtomicUsize,
        operator_surface: bool,
    }

    impl StubChannel {
        fn new(operator_surface: bool) -> Self {
            Self {
                asked: AtomicUsize::new(0),
                operator_surface,
            }
        }
    }

    impl Attributable for StubChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Cli)
        }
        fn alias(&self) -> &str {
            "stub-channel"
        }
    }

    #[async_trait]
    impl Channel for StubChannel {
        fn name(&self) -> &str {
            "stub"
        }
        fn is_operator_approval_surface(&self) -> bool {
            self.operator_surface
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
            self.asked.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ChannelApprovalResponse::Approve))
        }
    }

    struct StubTool {
        operator_only: bool,
    }

    impl Attributable for StubTool {
        fn role(&self) -> Role {
            Role::Tool(ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            "stub_tool"
        }
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            "stub_tool"
        }
        fn description(&self) -> &str {
            "test stub"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn approval_requires_operator(&self) -> bool {
            self.operator_only
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok("ok"))
        }
    }

    fn supervised_profile() -> RiskProfileConfig {
        RiskProfileConfig {
            level: zeroclaw_config::autonomy::AutonomyLevel::Supervised,
            ..RiskProfileConfig::default()
        }
    }

    async fn run_gate(
        operator_only_tool: bool,
        operator_surface_channel: bool,
    ) -> (ApprovalGateOutcome, usize) {
        let mgr = ApprovalManager::for_non_interactive(&supervised_profile());
        let channel = StubChannel::new(operator_surface_channel);
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(StubTool {
            operator_only: operator_only_tool,
        })];
        let pacing = PacingConfig::default();
        let ctx = TurnCtx {
            observer: &crate::observability::NoopObserver,
            provider_name: "stub",
            model: "stub-model",
            temperature: None,
            approval: Some(&mgr),
            channel_name: "stub",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: Some(&channel),
            turn_id: "gate-test",
            agent_alias: None,
            parent_agent_alias: None,
            tools: &tools,
        };
        let outcome = gate_tool_approval(&ctx, "stub_tool", &serde_json::json!({"x": 1}), 0).await;
        (outcome, channel.asked.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_chat_channel_is_never_asked_to_approve_an_operator_only_tool() {
        let (outcome, asked) = run_gate(true, false).await;

        assert_eq!(asked, 0, "the channel must not receive the approval prompt");
        match outcome {
            ApprovalGateOutcome::Deny(result) => {
                assert!(
                    result.output.contains("cannot be approved"),
                    "the deny explains itself instead of claiming a user denied it: {}",
                    result.output
                );
            }
            _ => panic!("an unapprovable call must be denied"),
        }
    }

    #[tokio::test]
    async fn an_operator_surface_channel_still_approves_operator_only_tools() {
        // A paired gateway client IS the operator; blocking it would break
        // the desktop/web frontends the tool exists to serve.
        let (outcome, asked) = run_gate(true, true).await;

        assert_eq!(asked, 1, "the operator surface is asked normally");
        assert!(
            matches!(outcome, ApprovalGateOutcome::Proceed { approved: true }),
            "its Approve answer stands"
        );
    }

    #[tokio::test]
    async fn ordinary_tools_keep_the_channel_inline_approval_path() {
        let (outcome, asked) = run_gate(false, false).await;

        assert_eq!(asked, 1, "a non-operator-only tool is asked as before");
        assert!(matches!(
            outcome,
            ApprovalGateOutcome::Proceed { approved: true }
        ));
    }
}
