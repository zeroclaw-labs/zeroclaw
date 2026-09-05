//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::approval::{ApprovalRequest, ApprovalRequirement, ApprovalResponse};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
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
    let session_prompt_mutation = is_session_prompt_mutation(tool_name);
    if session_prompt_mutation && ctx.session_prompt_approval_required {
        return gate_session_prompt_approval(ctx, tool_name, tool_args).await;
    }

    let mut approval_requirement = ctx
        .approval
        .map(|mgr| mgr.approval_requirement(tool_name))
        .unwrap_or(ApprovalRequirement::NotRequired);
    if let Some(mgr) = ctx.approval
        && approval_requirement == ApprovalRequirement::Prompt
    {
        // The relaxed session-prompt policy skips only the dedicated exact
        // confirmation. Ordinary approval and audit surfaces remain exports,
        // so they receive stable metadata rather than attachment content.
        let approval_args = generic_approval_arguments(tool_name, tool_args);
        let request = ApprovalRequest {
            tool_name: tool_name.to_string(),
            arguments: approval_args.clone(),
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
        // A replacement is provider-visible working context, but the approval
        // audit is an export. Preserve the decision kind without retaining a
        // replacement body for a session-prompt mutation.
        let audit_decision = if session_prompt_mutation {
            match decision {
                ApprovalResponse::ReplaceWith(_) => ApprovalResponse::ReplaceWith(
                    "[Session-prompt replacement omitted from export]".to_string(),
                ),
                _ => decision.clone(),
            }
        } else {
            decision.clone()
        };
        mgr.record_decision(
            tool_name,
            &approval_args,
            &audit_decision,
            &decision_channel,
        );

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
                        "arguments": scrub_credentials(&approval_args.to_string()),
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
                        "arguments": scrub_credentials(&approval_args.to_string()),
                        "replaced": true,
                        "output": if session_prompt_mutation {
                            "[Session-prompt replacement omitted from export]".to_string()
                        } else {
                            scrub_credentials(replacement)
                        },
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

fn is_session_prompt_mutation(tool_name: &str) -> bool {
    matches!(tool_name, "session_prompt_set" | "session_prompt_delete")
}

/// Render arguments for ordinary approval and audit exports. The set tool's
/// body is model-visible only through the provider context, explicit list
/// result, and exact one-time confirmation; a relaxed confirmation policy does
/// not make the body safe for generic approval sinks.
fn generic_approval_arguments(tool_name: &str, tool_args: &serde_json::Value) -> serde_json::Value {
    if !is_session_prompt_mutation(tool_name) {
        return tool_args.clone();
    }

    let raw_id = tool_args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let attachment_id = zeroclaw_infra::session_prompts::validate_prompt_id(raw_id).ok();
    let action = if tool_name == "session_prompt_set" {
        "set"
    } else {
        "delete"
    };
    let content_sha256 = if action == "set" {
        tool_args
            .get("content")
            .and_then(serde_json::Value::as_str)
            // Validate before hashing so an impossible mutation is never
            // rendered or digested for an approval request.
            .and_then(|content| {
                zeroclaw_infra::session_prompts::validate_prompt(raw_id, content)
                    .ok()
                    .map(|_| format!("{:x}", Sha256::digest(content.as_bytes())))
            })
    } else {
        None
    };

    serde_json::json!({
        "storage_domain": "sqlite chat session prompts",
        "action": action,
        "attachment_id": attachment_id,
        "content_sha256": content_sha256,
    })
}

fn session_prompt_approval_summary(
    tool_name: &str,
    tool_args: &serde_json::Value,
) -> Result<String, &'static str> {
    let session_id = zeroclaw_api::TOOL_LOOP_SESSION_KEY
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .ok_or("no active chat session is available for confirmation")?;
    let raw_id = tool_args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or("the attachment id is missing")?;
    // Storage accepts only this canonical representation. Validate before the
    // operator sees the binding so approval and execution cannot disagree.
    let id = zeroclaw_infra::session_prompts::validate_prompt_id(raw_id)
        .map_err(|_| "the attachment id is invalid")?;
    let action = if tool_name == "session_prompt_set" {
        "set"
    } else {
        "delete"
    };
    let mut summary = String::from(
        "Approve this one persistent session-prompt mutation. This approval cannot be remembered.\n",
    );
    let _ = writeln!(summary, "action: {action}");
    let _ = writeln!(summary, "storage_domain: sqlite chat session prompts");
    let _ = writeln!(
        summary,
        "session_id: {}",
        escape_prompt_preview(&session_id)
    );
    let _ = writeln!(summary, "attachment_id: {id}");
    if action == "set" {
        let content = tool_args
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or("the prompt content is missing")?;
        // Keep approval work below the same content bound as persistence. This
        // avoids hashing or rendering an oversized request that cannot run.
        zeroclaw_infra::session_prompts::validate_prompt(&id, content)
            .map_err(|_| "the prompt content is invalid")?;
        let digest = Sha256::digest(content.as_bytes());
        let _ = writeln!(summary, "content_sha256: {digest:x}");
        let _ = writeln!(
            summary,
            "content_escaped: {}",
            escape_prompt_preview(content)
        );
    }
    Ok(summary)
}

/// Render untrusted prompt text for an approval surface without allowing
/// terminal controls or invisible direction changes to affect the display.
fn escape_prompt_preview(content: &str) -> String {
    let mut escaped = String::with_capacity(content.len() + 2);
    escaped.push('"');
    for ch in content.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control()
                || matches!(
                    ch,
                    '\u{200e}'
                    | '\u{200f}'
                        | '\u{061c}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                ) =>
            {
                let _ = write!(escaped, "\\u{{{:04X}}}", ch as u32);
            }
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

async fn gate_session_prompt_approval(
    ctx: &TurnCtx<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
) -> ApprovalGateOutcome {
    let denied = |reason: &str| {
        ApprovalGateOutcome::Deny(ToolExecutionOutcome {
            output: format!("Session prompt mutation not executed: {reason}"),
            success: false,
            error_reason: Some("session prompt mutation denied".to_string()),
            duration: Duration::ZERO,
            receipt: None,
            output_data: None,
        })
    };
    let Ok(summary) = session_prompt_approval_summary(tool_name, tool_args) else {
        return denied("the runtime could not bind an exact session confirmation");
    };
    let Some(mgr) = ctx.approval else {
        return denied("no approval manager is available");
    };

    let (approved, decision_channel) = if mgr.is_non_interactive() {
        let Some(channel) = ctx.channel else {
            return denied("no approval-capable channel is available");
        };
        let request = zeroclaw_api::channel::ChannelApprovalRequest {
            tool_name: tool_name.to_string(),
            arguments_summary: summary,
            // Prompt content belongs only on the approval surface, never in the
            // generic structured arguments that downstream event consumers log.
            raw_arguments: None,
        };
        let approved = match channel
            .request_approval_attributed(ctx.channel_reply_target.unwrap_or_default(), &request)
            .await
        {
            Ok(Some(attributed)) => is_one_time_session_prompt_approval(&attributed),
            Ok(_) | Err(_) => false,
        };
        (approved, ctx.channel_name)
    } else {
        (
            mgr.prompt_cli_once(
                &crate::i18n::get_required_cli_string("session-prompt-approval-heading"),
                &summary,
            ),
            ctx.channel_name,
        )
    };

    let audit_args = generic_approval_arguments(tool_name, tool_args);
    mgr.record_decision(
        tool_name,
        &audit_args,
        &if approved {
            crate::approval::ApprovalResponse::Yes
        } else {
            crate::approval::ApprovalResponse::No
        },
        decision_channel,
    );

    if approved {
        ApprovalGateOutcome::Proceed { approved: true }
    } else {
        denied("a one-time operator approval was not granted")
    }
}

fn is_one_time_session_prompt_approval(
    approval: &zeroclaw_api::channel::AttributedApprovalResponse,
) -> bool {
    !approval.source.is_runtime_fail_closed()
        && matches!(
            approval.response,
            zeroclaw_api::channel::ChannelApprovalResponse::Approve
        )
}

#[cfg(test)]
mod tests {
    use super::{
        escape_prompt_preview, gate_tool_approval, generic_approval_arguments,
        is_one_time_session_prompt_approval, session_prompt_approval_summary,
    };
    use crate::agent::turn::context::TurnCtx;
    use crate::approval::ApprovalManager;
    use crate::observability::NoopObserver;
    use parking_lot::Mutex;
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
    use zeroclaw_api::channel::{
        ApprovalSource, AttributedApprovalResponse, Channel, ChannelApprovalRequest,
        ChannelApprovalResponse, ChannelMessage, SendMessage,
    };
    use zeroclaw_config::schema::{PacingConfig, StreamReasoningMode};

    struct CapturingApprovalChannel {
        response: Option<ChannelApprovalResponse>,
        requests: Mutex<Vec<ChannelApprovalRequest>>,
    }

    impl Attributable for CapturingApprovalChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Webhook)
        }

        fn alias(&self) -> &str {
            "capturing-approval"
        }
    }

    #[async_trait::async_trait]
    impl Channel for CapturingApprovalChannel {
        fn name(&self) -> &str {
            "capturing-approval"
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
            request: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            self.requests.lock().push(request.clone());
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn generic_approval_exports_metadata_not_prompt_body_for_every_decision() {
        let marker = "session-prompt-private-marker";
        for response in [
            Some(ChannelApprovalResponse::Approve),
            Some(ChannelApprovalResponse::Deny),
            Some(ChannelApprovalResponse::DenyWithEdit {
                replacement: format!("replacement: {marker}"),
            }),
            None,
        ] {
            let channel = CapturingApprovalChannel {
                response,
                requests: Mutex::new(Vec::new()),
            };
            let approval = ApprovalManager::for_non_interactive_backchannel(
                &zeroclaw_config::schema::RiskProfileConfig::default(),
            );
            let observer = NoopObserver;
            let pacing = PacingConfig::default();
            let ctx = TurnCtx {
                observer: &observer,
                provider_name: "test-provider",
                model: "test-model",
                temperature: None,
                approval: Some(&approval),
                // Exercise the relaxed dedicated-confirmation policy. Ordinary
                // supervised approval must still redact its generic exports.
                session_prompt_approval_required: false,
                channel_name: "test-channel",
                channel_reply_target: Some("operator"),
                cancellation_token: None,
                on_delta: None,
                event_tx: None,
                hooks: None,
                dedup_exempt_tools: &[],
                pacing: &pacing,
                strict_tool_parsing: false,
                channel: Some(&channel),
                draft_reasoning: StreamReasoningMode::Off,
                turn_id: "test-turn",
                agent_alias: None,
                parent_agent_alias: None,
            };

            let _ = gate_tool_approval(
                &ctx,
                "session_prompt_set",
                &serde_json::json!({"id": "task", "content": marker}),
                0,
            )
            .await;

            let requests = channel.requests.lock();
            assert_eq!(requests.len(), 1);
            let request = &requests[0];
            assert!(
                !request.arguments_summary.contains(marker)
                    && !request
                        .raw_arguments
                        .as_ref()
                        .is_some_and(|arguments| arguments.to_string().contains(marker)),
                "generic channel approval must not receive the opaque body"
            );
            assert_eq!(
                request.raw_arguments.as_ref().unwrap()["attachment_id"],
                "task"
            );
            assert!(request.raw_arguments.as_ref().unwrap()["content_sha256"].is_string());
            assert!(
                approval.audit_log().iter().all(|entry| {
                    !entry.arguments_summary.contains(marker)
                        && !format!("{:?}", entry.decision).contains(marker)
                }),
                "approval audit must not receive the opaque body"
            );
        }
    }

    #[tokio::test]
    async fn session_prompt_confirmation_binds_session_id_content_and_digest() {
        let summary = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(
                Some("matrix:room:thread".to_string()),
                async {
                    session_prompt_approval_summary(
                        "session_prompt_set",
                        &serde_json::json!({"id": "current-task", "content": "Finish RFC reconciliation."}),
                    )
                    .unwrap()
                },
            )
            .await;

        assert!(summary.contains("action: set"));
        assert!(summary.contains("storage_domain: sqlite chat session prompts"));
        assert!(summary.contains("session_id: \"matrix:room:thread\""));
        assert!(summary.contains("attachment_id: current-task"));
        assert!(summary.contains("content_escaped: \"Finish RFC reconciliation.\""));
        assert!(summary.contains(
            "content_sha256: 16e48f498e379a0e5530eb194069ef5ce3f2133b53b6bdd28c80472425e552de"
        ));
    }

    #[tokio::test]
    async fn session_prompt_confirmation_escapes_control_characters() {
        let summary = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("session".to_string()), async {
                session_prompt_approval_summary(
                    "session_prompt_set",
                    &serde_json::json!({"id": "task", "content": "first\n\u{001b}[2J"}),
                )
                .unwrap()
            })
            .await;

        assert!(summary.contains("content_escaped: \"first\\n\\u{001B}[2J\""));
        assert!(!summary.contains('\u{001b}'));
    }

    #[tokio::test]
    async fn session_prompt_confirmation_escapes_session_identifier_controls() {
        let summary = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("session\n\u{001b}[2J\u{202e}x".to_string()), async {
                session_prompt_approval_summary(
                    "session_prompt_delete",
                    &serde_json::json!({"id": "task"}),
                )
            })
            .await
            .expect("the confirmation summary should render safely");

        assert!(summary.contains("session_id: \"session\\n\\u{001B}[2J\\u{202E}x\""));
        assert!(!summary.contains('\u{001b}'));
        assert!(!summary.contains('\u{202e}'));
    }

    #[test]
    fn session_prompt_preview_escapes_c1_and_bidi_controls() {
        let preview = escape_prompt_preview("a\u{009b}2J\u{061c}\u{202e}txt");
        assert_eq!(preview, "\"a\\u{009B}2J\\u{061C}\\u{202E}txt\"");
        assert!(!preview.contains('\u{009b}'));
        assert!(!preview.contains('\u{061c}'));
        assert!(!preview.contains('\u{202e}'));
    }

    #[test]
    fn session_prompt_confirmation_rejects_persistent_and_runtime_decisions() {
        assert!(is_one_time_session_prompt_approval(
            &AttributedApprovalResponse::operator(ChannelApprovalResponse::Approve)
        ));
        assert!(!is_one_time_session_prompt_approval(
            &AttributedApprovalResponse::operator(ChannelApprovalResponse::AlwaysApprove)
        ));
        assert!(!is_one_time_session_prompt_approval(
            &AttributedApprovalResponse::from_runtime(
                ChannelApprovalResponse::Approve,
                ApprovalSource::TimedOut,
            )
        ));
    }

    #[test]
    fn generic_session_prompt_approval_arguments_keep_identity_without_content() {
        let marker = "session-prompt-private-marker";
        let args = generic_approval_arguments(
            "session_prompt_set",
            &serde_json::json!({"id": "current-task", "content": marker}),
        );

        assert_eq!(args["action"], "set");
        assert_eq!(args["attachment_id"], "current-task");
        assert!(args["content_sha256"].as_str().is_some());
        assert!(!args.to_string().contains(marker));
    }

    #[test]
    fn generic_session_prompt_approval_arguments_do_not_hash_invalid_content() {
        let marker = "x".repeat(zeroclaw_infra::session_prompts::MAX_SESSION_PROMPT_BYTES + 1);
        let args = generic_approval_arguments(
            "session_prompt_set",
            &serde_json::json!({"id": "current-task", "content": marker}),
        );

        assert!(args["content_sha256"].is_null());
        assert!(!args.to_string().contains(&marker));
    }

    #[tokio::test]
    async fn session_prompt_confirmation_canonicalizes_attachment_ids() {
        let result = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("matrix:room:thread".to_string()), async {
                session_prompt_approval_summary(
                    "session_prompt_delete",
                    &serde_json::json!({"id": " task "}),
                )
            })
            .await;
        assert!(result.unwrap().contains("attachment_id: task"));
    }
}
