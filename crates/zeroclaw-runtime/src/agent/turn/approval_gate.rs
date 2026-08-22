//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::{loggable_args_string, scrub_credentials};
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
    tool_args: &mut serde_json::Value,
    iteration: usize,
) -> ApprovalGateOutcome {
    // This namespace belongs to the host. A model or modifying hook cannot
    // smuggle a binding into execution: hooks run before this gate and every
    // caller-provided value is removed before the tool creates a fresh one.
    if let Some(args) = tool_args.as_object_mut() {
        args.remove(zeroclaw_api::tool::APPROVAL_EXECUTION_BINDING_ARG);
    }
    let mut approval_requirement = ctx
        .approval
        .map(|mgr| mgr.approval_requirement(tool_name))
        .unwrap_or(ApprovalRequirement::NotRequired);
    if let Some(mgr) = ctx.approval
        && approval_requirement == ApprovalRequirement::Prompt
    {
        let tool = ctx.tool_by_name(tool_name);
        let host_summary = tool
            .and_then(|tool| tool.approval_summary_for_call(tool_args))
            .and_then(|summary| {
                if let Some(binding) = summary.execution_binding {
                    let args = tool_args.as_object_mut()?;
                    args.insert(
                        zeroclaw_api::tool::APPROVAL_EXECUTION_BINDING_ARG.to_string(),
                        binding,
                    );
                }
                Some(summary.text)
            });
        let request = ApprovalRequest {
            tool_name: tool_name.to_string(),
            arguments: tool_args.clone(),
            // Host-computed from the arguments' effects by the tool itself;
            // the model cannot author what the operator reads here.
            host_summary,
        };

        // Interactive CLI: prompt the operator.
        // Non-interactive (channels): try the channel's inline
        // approval (e.g. Telegram inline keyboard) before falling
        // back to auto-deny.
        // A tool that promises a secret-aware, effects-based operator prompt
        // (config authoring) must not fall back to the generic argument summary
        // when that prompt cannot be produced: the raw arguments may carry
        // secrets, and approving blind is itself unsafe. Refuse closed.
        let summary_required_but_missing = request.host_summary.is_none()
            && tool.is_some_and(|t| t.requires_host_approval_summary());
        // Redacted arguments for every non-execution sink (approval audit,
        // WARN records, channel frame). `redact_args_for_log` masks secret op
        // values at the source; for tools that do not opt in it returns the
        // arguments unchanged.
        let redacted_args = tool
            .and_then(|t| t.redact_args_for_log(tool_args))
            .unwrap_or_else(|| tool_args.clone());

        let mut operator_only_channel_refused = false;
        let (decision, decided_by, unanswerable) = if summary_required_but_missing {
            (ApprovalResponse::No, None, true)
        } else if mgr.is_non_interactive() {
            let operator_only = tool.is_some_and(|tool| tool.approval_requires_operator());
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
                            .unwrap_or_else(|| crate::approval::summarize_args(&redacted_args)),
                        // The channel frame crosses to a remote client; never
                        // ship the raw (possibly secret-bearing) arguments.
                        raw_arguments: Some(redacted_args.clone()),
                    };
                    let recipient = ctx.channel_reply_target.unwrap_or_default();
                    let response = if operator_only {
                        ch.request_operator_approval_attributed(recipient, &ch_request)
                            .await
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
        // Audit with the redacted arguments: `record_decision` runs
        // `summarize_args`, which is not path-aware and would otherwise retain
        // a short nested secret value.
        mgr.record_decision(tool_name, &redacted_args, &decision, &decision_channel);

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
            let denied = if summary_required_but_missing {
                format!(
                    "Tool call not executed: '{tool_name}' requires an operator approval \
                     preview that could not be produced (the configuration could not be read \
                     or the requested change does not apply). Not approved."
                )
            } else if operator_only_channel_refused {
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
                        "arguments": loggable_args_string(tool, tool_args),
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
                        "arguments": loggable_args_string(tool, tool_args),
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
    use zeroclaw_config::schema::{PacingConfig, RiskProfileConfig, StreamReasoningMode};

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

    #[derive(Default)]
    struct StubTool {
        operator_only: bool,
        requires_summary: bool,
        summary: Option<String>,
        execution_binding: Option<serde_json::Value>,
        /// When true, `redact_args_for_log` masks each nested `ops[].value`,
        /// mirroring how `config_patch` redacts a secret that sits under an
        /// innocuously-named key the generic summary would not catch.
        redacts_secret: bool,
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
        fn requires_host_approval_summary(&self) -> bool {
            self.requires_summary
        }
        fn approval_summary(&self, _args: &serde_json::Value) -> Option<String> {
            self.summary.clone()
        }
        fn approval_summary_for_call(
            &self,
            args: &serde_json::Value,
        ) -> Option<zeroclaw_api::tool::ToolApprovalSummary> {
            let text = self.approval_summary(args)?;
            Some(match self.execution_binding.clone() {
                Some(binding) => {
                    zeroclaw_api::tool::ToolApprovalSummary::with_execution_binding(text, binding)
                }
                None => zeroclaw_api::tool::ToolApprovalSummary::new(text),
            })
        }
        fn redact_args_for_log(&self, args: &serde_json::Value) -> Option<serde_json::Value> {
            if !self.redacts_secret {
                return None;
            }
            let mut redacted = args.clone();
            if let Some(ops) = redacted.get_mut("ops").and_then(|v| v.as_array_mut()) {
                for op in ops.iter_mut() {
                    if let Some(obj) = op.as_object_mut()
                        && obj.contains_key("value")
                    {
                        obj.insert("value".into(), serde_json::json!("[redacted]"));
                    }
                }
            }
            Some(redacted)
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
            ..Default::default()
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
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "gate-test",
            agent_alias: None,
            parent_agent_alias: None,
            tools: &tools,
        };
        let mut args = serde_json::json!({"x": 1});
        let outcome = gate_tool_approval(&ctx, "stub_tool", &mut args, 0).await;
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

    /// Drive the gate with a specific stub tool and arguments; return the
    /// outcome, the audit-log arguments summaries, and how often the channel
    /// was asked.
    async fn run_gate_with(
        tool: StubTool,
        mut args: serde_json::Value,
        operator_surface_channel: bool,
    ) -> (ApprovalGateOutcome, Vec<String>, usize, serde_json::Value) {
        let mgr = ApprovalManager::for_non_interactive(&supervised_profile());
        let channel = StubChannel::new(operator_surface_channel);
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(tool)];
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
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "gate-test",
            agent_alias: None,
            parent_agent_alias: None,
            tools: &tools,
        };
        let outcome = gate_tool_approval(&ctx, "stub_tool", &mut args, 0).await;
        let audit = mgr
            .audit_log()
            .into_iter()
            .map(|e| e.arguments_summary)
            .collect();
        (outcome, audit, channel.asked.load(Ordering::SeqCst), args)
    }

    /// A tool that requires a host-computed, secret-aware approval summary must
    /// be refused — never asked, never shown the generic argument fallback —
    /// when that summary cannot be produced.
    #[tokio::test]
    async fn refuses_when_a_required_host_summary_is_unavailable() {
        let tool = StubTool {
            operator_only: true,
            requires_summary: true,
            summary: None,
            ..Default::default()
        };
        let (outcome, _audit, asked, _args) =
            run_gate_with(tool, serde_json::json!({"x": 1}), true).await;

        assert_eq!(
            asked, 0,
            "a tool whose required summary is missing must not be prompted anywhere"
        );
        match outcome {
            ApprovalGateOutcome::Deny(result) => assert!(
                result.output.contains("could not be produced"),
                "the refusal explains the missing preview: {}",
                result.output
            ),
            _ => panic!("must be denied when the required summary is unavailable"),
        }
    }

    /// A secret carried in the arguments must not survive into the approval
    /// audit: `record_decision` runs the non-path-aware `summarize_args`, so
    /// the gate must hand it the tool-redacted arguments.
    #[tokio::test]
    async fn the_approval_audit_never_retains_a_redacted_secret() {
        let tool = StubTool {
            requires_summary: false,
            redacts_secret: true,
            ..Default::default()
        };
        // The secret sits under the nested `value` key — not a top-level
        // secret-named key the generic summary would auto-redact — and early
        // enough in the stringified ops to survive the 80-char truncation.
        let (outcome, audit, _asked, _args) = run_gate_with(
            tool,
            serde_json::json!({"ops": [
                {"op": "add", "path": "/x", "value": "sentinel-never-logged-01234567"}
            ]}),
            true,
        )
        .await;

        assert!(
            matches!(outcome, ApprovalGateOutcome::Proceed { approved: true }),
            "the non-operator-only tool is approved by the channel"
        );
        for summary in &audit {
            assert!(
                !summary.contains("sentinel-never-logged"),
                "the raw secret leaked into the approval audit: {summary}"
            );
        }
        assert!(!audit.is_empty(), "the decision was recorded");
    }

    #[tokio::test]
    async fn model_supplied_approval_binding_is_replaced_by_the_host() {
        let tool = StubTool {
            requires_summary: true,
            summary: Some("host preview".to_string()),
            execution_binding: Some(serde_json::json!("host-authenticated-binding")),
            ..Default::default()
        };
        let mut supplied_args = serde_json::json!({"x": 1});
        supplied_args.as_object_mut().expect("object args").insert(
            zeroclaw_api::tool::APPROVAL_EXECUTION_BINDING_ARG.to_string(),
            serde_json::json!("model-forged-binding"),
        );
        let (outcome, _audit, asked, args) = run_gate_with(tool, supplied_args, true).await;

        assert!(matches!(
            outcome,
            ApprovalGateOutcome::Proceed { approved: true }
        ));
        assert_eq!(asked, 1);
        assert_eq!(
            args[zeroclaw_api::tool::APPROVAL_EXECUTION_BINDING_ARG],
            "host-authenticated-binding",
            "the model-controlled value must never reach execution"
        );
    }
}
