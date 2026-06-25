//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::approval::{ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use zeroclaw_api::observability_traits::{AuthorizationResponseType, ObserverEvent};
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

        // Emit AuthorizationRequested before prompting
        let arguments_summary = crate::approval::summarize_args(tool_args);
        let event = ObserverEvent::AuthorizationRequested {
            tool_name: tool_name.to_string(),
            arguments_summary,
            channel: Some(ctx.channel_name.to_string()),
            agent_alias: None,
            turn_id: Some(ctx.turn_id.to_string()),
        };
        ctx.observer.record_event(&event);

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

        // Emit AuthorizationResponded event
        let response_type = match decision {
            ApprovalResponse::Yes => AuthorizationResponseType::Yes,
            ApprovalResponse::Always => AuthorizationResponseType::Always,
            ApprovalResponse::No => AuthorizationResponseType::No,
            ApprovalResponse::ReplaceWith(_) => AuthorizationResponseType::ReplaceWith,
        };
        let event = ObserverEvent::AuthorizationResponded {
            tool_name: tool_name.to_string(),
            granted: matches!(decision, ApprovalResponse::Yes | ApprovalResponse::Always),
            response_type,
            channel: Some(ctx.channel_name.to_string()),
            agent_alias: None,
            turn_id: Some(ctx.turn_id.to_string()),
        };
        ctx.observer.record_event(&event);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::ApprovalManager;
    use crate::integrations::herdr::tests::{HerdrSpy, HerdrSpyCall, make_spy_reporter};
    use crate::integrations::herdr::{DebouncedReporter, HerdrObserver, HerdrState};
    use crate::observability::{set_scoped_broadcast_hook, clear_broadcast_hook};
    use crate::agent::turn::context::TurnCtx;
    use zeroclaw_api::channel::{Channel, ChannelApprovalRequest, ChannelApprovalResponse};
    use zeroclaw_api::observability_traits::Observer;
    use zeroclaw_config::schema::{RiskProfileConfig, PacingConfig, AutonomyLevel};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    /// A mock channel that approves all requests.
    struct ApprovingChannel {
        approval_requests: Arc<AtomicUsize>,
    }

    impl ApprovingChannel {
        fn new(approval_requests: Arc<AtomicUsize>) -> Self {
            Self { approval_requests }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ApprovingChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
            )
        }
        fn alias(&self) -> &str { "approving-test" }
    }

    #[async_trait::async_trait]
    impl Channel for ApprovingChannel {
        fn name(&self) -> &str { "approving-test" }

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
            _request: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            self.approval_requests.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ChannelApprovalResponse::Approve))
        }
    }

    /// A mock channel that denies all requests.
    struct DenyingChannel {
        approval_requests: Arc<AtomicUsize>,
    }

    impl DenyingChannel {
        fn new(approval_requests: Arc<AtomicUsize>) -> Self {
            Self { approval_requests }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for DenyingChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
            )
        }
        fn alias(&self) -> &str { "denying-test" }
    }

    #[async_trait::async_trait]
    impl Channel for DenyingChannel {
        fn name(&self) -> &str { "denying-test" }

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
            _request: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            self.approval_requests.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ChannelApprovalResponse::Deny))
        }
    }

    fn make_turn_ctx<'a>(
        observer: &'a dyn Observer,
        approval: Option<&'a ApprovalManager>,
        channel: Option<&'a dyn Channel>,
    ) -> TurnCtx<'a> {
        let (tx, _rx) = mpsc::channel(8);
        TurnCtx {
            observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: None,
            approval,
            channel_name: "test-channel",
            channel_reply_target: Some("user"),
            cancellation_token: None,
            on_delta: Some(&tx),
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing: &PacingConfig::default(),
            strict_tool_parsing: false,
            channel,
            turn_id: "turn-1",
        }
    }

    #[test]
    fn approval_gate_herdr_approve_transitions_blocked_to_working() {
        clear_broadcast_hook();

        // 1. Create spy and HerdrObserver
        let spy = HerdrSpy::new();
        let (reporter, calls) = make_spy_reporter(spy);
        let herdr_observer = Arc::new(HerdrObserver::new_with_reporter(reporter));
        let _guard = set_scoped_broadcast_hook(herdr_observer.clone());

        // 2. Session ID for report_agent payload
        crate::integrations::herdr::update_session_id("test-session-1");

        // 3. Non-interactive backchannel approval manager (shell in always_ask)
        let risk = RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            always_ask: vec!["shell".into()],
            ..Default::default()
        };
        let approval = ApprovalManager::for_non_interactive_backchannel(&risk);

        // 4. Mock channel that approves
        let approval_count = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(ApprovingChannel::new(approval_count.clone())) as Arc<dyn Channel>;

        // 5. Build TurnCtx
        let ctx = make_turn_ctx(&*herdr_observer, Some(&approval), Some(&*channel));

        // 6. Call gate_tool_approval
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(gate_tool_approval(&ctx, "shell", &serde_json::json!({"cmd": "echo hi"}), 0));

        // 7. Verify outcome
        assert!(matches!(outcome, ApprovalGateOutcome::Proceed { approved: true }));

        // 8. Verify HerdrObserver state transitions: Blocked -> Working
        let captured = calls.lock().drain(..).collect::<Vec<HerdrSpyCall>>();
        let state_sequence: Vec<&str> = captured.iter()
            .filter(|c| c.method == "pane.report_agent")
            .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
            .collect();

        assert_eq!(state_sequence, vec!["blocked", "working"],
            "Expected state sequence [blocked, working], got {:?}", state_sequence);
    }

    #[test]
    fn approval_gate_herdr_deny_transitions_blocked_to_idle() {
        clear_broadcast_hook();

        // 1. Create spy and HerdrObserver
        let spy = HerdrSpy::new();
        let (reporter, calls) = make_spy_reporter(spy);
        let herdr_observer = Arc::new(HerdrObserver::new_with_reporter(reporter));
        let _guard = set_scoped_broadcast_hook(herdr_observer.clone());

        // 2. Session ID
        crate::integrations::herdr::update_session_id("test-session-2");

        // 3. Non-interactive backchannel approval manager
        let risk = RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            always_ask: vec!["shell".into()],
            ..Default::default()
        };
        let approval = ApprovalManager::for_non_interactive_backchannel(&risk);

        // 4. Mock channel that denies
        let approval_count = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(DenyingChannel::new(approval_count.clone())) as Arc<dyn Channel>;

        // 5. Build TurnCtx
        let ctx = make_turn_ctx(&*herdr_observer, Some(&approval), Some(&*channel));

        // 6. Call gate_tool_approval
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(gate_tool_approval(&ctx, "shell", &serde_json::json!({"cmd": "rm -rf /"}), 0));

        // 7. Verify outcome is Deny
        assert!(matches!(outcome, ApprovalGateOutcome::Deny(_)));

        // 8. Verify HerdrObserver state transitions: Blocked -> Idle
        let captured = calls.lock().drain(..).collect::<Vec<HerdrSpyCall>>();
        let state_sequence: Vec<&str> = captured.iter()
            .filter(|c| c.method == "pane.report_agent")
            .filter_map(|c| c.params.get("state").and_then(|s| s.as_str()))
            .collect();

        assert_eq!(state_sequence, vec!["blocked", "idle"],
            "Expected state sequence [blocked, idle], got {:?}", state_sequence);
    }
}
