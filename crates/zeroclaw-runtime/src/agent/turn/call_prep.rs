//! The per-call preparation loop: `before_tool_call` hook, delivery defaults,
//! the approval gate, the duplicate-call gate, and start logging — producing
//! the executable subset of this round's tool calls.

use super::approval_gate::{ApprovalGateOutcome, gate_tool_approval};
use super::context::TurnCtx;
use super::delivery_defaults::maybe_inject_channel_delivery_defaults;
use super::events::{StreamDelta, emit_tool_call_pair};
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::collections::HashSet;
use std::time::Duration;
use zeroclaw_tool_call_parser::{ParsedToolCall, canonicalize_json_for_tool_signature};

static FRESH_CONFIRMATION_EXECUTION_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// The prepared subset of one round's tool calls.
///
/// `ordered_results` has one slot per incoming call; prep fills the slots for
/// calls that never execute (hook-cancelled, denied, replaced, deduplicated)
/// and post-exec fills the rest, so result ordering always matches the
/// model's call ordering. `seen_tool_signatures` stays owned by the loop
/// skeleton (reset per iteration) and is threaded in as `&mut`.
pub(crate) struct PreparedToolCalls {
    pub(crate) ordered_results: Vec<Option<(String, Option<String>, ToolExecutionOutcome)>>,
    pub(crate) executable_indices: Vec<usize>,
    pub(crate) executable_calls: Vec<ParsedToolCall>,
    /// Process-wide lease acquired before a Fresh prompt and retained until
    /// the approved call finishes. This closes approval-to-execution races
    /// between concurrent turns and between different Fresh tools.
    pub(crate) fresh_confirmation_guard: Option<tokio::sync::MutexGuard<'static, ()>>,
}

fn tool_call_signature(tool_name: &str, tool_args: &serde_json::Value) -> (String, String) {
    let canonical_args = canonicalize_json_for_tool_signature(tool_args);
    let args_json = serde_json::to_string(&canonical_args).unwrap_or_else(|_| "{}".to_string());
    (tool_name.trim().to_ascii_lowercase(), args_json)
}

async fn record_rejected_tool_call(
    ctx: &TurnCtx<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    iteration: usize,
    reason: String,
    deduplicated: bool,
) -> ToolExecutionOutcome {
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
            .with_category(::zeroclaw_log::EventCategory::Tool)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "model": ctx.model,
                "iteration": iteration + 1,
                "tool": tool_name,
                "arguments": scrub_credentials(&tool_args.to_string()),
                "result": reason,
                "deduplicated": deduplicated,
                "trace_id": ctx.turn_id,
            })),
        "tool_call_result"
    );
    if let Some(tx) = ctx.on_delta {
        let _ = tx
            .send(StreamDelta::Status(format!(
                "\u{274c} {}: {}\n",
                tool_name, reason
            )))
            .await;
    }
    ToolExecutionOutcome {
        output: reason.clone(),
        audit_output: None,
        success: false,
        error_reason: Some(reason),
        duration: Duration::ZERO,
        receipt: None,
        output_data: None,
    }
}

/// Run per-call preparation over this round's parsed tool calls (upstream
/// loop body, per-call prep loop).
pub(crate) async fn prepare_tool_calls(
    ctx: &TurnCtx<'_>,
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn crate::tools::Tool>],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    seen_tool_signatures: &mut HashSet<(String, String)>,
    prompt_approval_tool_signatures: &mut HashSet<(String, String)>,
    iteration: usize,
    dedup_enabled: bool,
) -> Result<PreparedToolCalls> {
    let mut ordered_results: Vec<Option<(String, Option<String>, ToolExecutionOutcome)>> =
        (0..tool_calls.len()).map(|_| None).collect();
    let mut executable_indices: Vec<usize> = Vec::new();
    let mut executable_calls: Vec<ParsedToolCall> = Vec::new();
    let mut fresh_confirmation_guard = None;
    let mut prompt_approval_tool_signatures_this_round: HashSet<(String, String)> = HashSet::new();

    for (idx, call) in tool_calls.iter().enumerate() {
        // ── Hook: before_tool_call (modifying) ──────────
        let mut tool_name = call.name.clone();
        let mut tool_args = call.arguments.clone();
        if let Some(hooks) = ctx.hooks {
            match hooks
                .run_before_tool_call(tool_name.clone(), tool_args.clone())
                .await
            {
                crate::hooks::HookResult::Cancel(reason) => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel).with_category(::zeroclaw_log::EventCategory::Tool).with_attrs(::serde_json::json!({"tool": call.name, "reason": reason.to_string()})), "tool call cancelled by hook");
                    let cancelled = format!("Cancelled by hook: {reason}");
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel)
                            .with_category(::zeroclaw_log::EventCategory::Tool)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "model": ctx.model,
                                "iteration": iteration + 1,
                                "tool": call.name,
                                "arguments": scrub_credentials(&tool_args.to_string()),
                                "result": cancelled,
                                "trace_id": ctx.turn_id,
                            })),
                        "tool_call_result"
                    );
                    if let Some(tx) = ctx.on_delta {
                        let _ = tx
                            .send(StreamDelta::Status(format!(
                                "\u{274c} {}: {}\n",
                                call.name,
                                truncate_with_ellipsis(&scrub_credentials(&cancelled), 200)
                            )))
                            .await;
                    }
                    let outcome = ToolExecutionOutcome {
                        output: cancelled,
                        audit_output: None,
                        success: false,
                        error_reason: Some(reason),
                        duration: Duration::ZERO,
                        receipt: None,
                        output_data: None,
                    };
                    // Streaming consumers still see the call and its
                    // hook-cancel outcome as a ToolCall/ToolResult pair,
                    // as the direct execution path always emitted.
                    if let Some(tx) = ctx.event_tx {
                        emit_tool_call_pair(tx, call, &outcome).await;
                    }
                    ordered_results[idx] =
                        Some((call.name.clone(), call.tool_call_id.clone(), outcome));
                    continue;
                }
                crate::hooks::HookResult::Continue((name, args)) => {
                    tool_name = name;
                    tool_args = args;
                }
            }
        }

        maybe_inject_channel_delivery_defaults(
            &tool_name,
            &mut tool_args,
            ctx.channel_name,
            ctx.channel_reply_target,
        );

        crate::agent::set_runtime_approved_arg(&tool_name, &mut tool_args, false);
        crate::agent::set_computer_use_approval_context(&tool_name, &mut tool_args, None);

        let confirmation = crate::agent::tool_execution::confirmation_for_call(
            tools_registry,
            activated_tools,
            &tool_name,
            &tool_args,
        );
        let confirmation_requirement = confirmation.requirement;
        crate::agent::set_fresh_confirmation_approved_arg(
            confirmation_requirement,
            &mut tool_args,
            false,
        );
        let confirmation_args = confirmation.effective_arguments;

        if confirmation_requirement == zeroclaw_api::tool::ConfirmationRequirement::Fresh
            && !crate::approval::confirmation_args_are_review_safe(&confirmation_args)
        {
            let rejected =
                crate::i18n::get_required_cli_string("cli-fresh-confirmation-unsafe-arguments");
            let outcome = record_rejected_tool_call(
                ctx,
                &tool_name,
                &confirmation_args,
                iteration,
                rejected,
                false,
            )
            .await;
            if let Some(tx) = ctx.event_tx {
                emit_tool_call_pair(tx, call, &outcome).await;
            }
            ordered_results[idx] = Some((tool_name.clone(), call.tool_call_id.clone(), outcome));
            continue;
        }

        if confirmation_requirement == zeroclaw_api::tool::ConfirmationRequirement::Fresh
            && tool_calls.len() != 1
        {
            let rejected = crate::i18n::get_required_cli_string_with_args(
                "cli-fresh-confirmation-single-call",
                &[("tool", &tool_name)],
            );
            let outcome = record_rejected_tool_call(
                ctx,
                &tool_name,
                &confirmation_args,
                iteration,
                rejected,
                false,
            )
            .await;
            if let Some(tx) = ctx.event_tx {
                emit_tool_call_pair(tx, call, &outcome).await;
            }
            ordered_results[idx] = Some((tool_name.clone(), call.tool_call_id.clone(), outcome));
            continue;
        }

        let call_fresh_confirmation_guard =
            if confirmation_requirement == zeroclaw_api::tool::ConfirmationRequirement::Fresh {
                match FRESH_CONFIRMATION_EXECUTION_LOCK.try_lock() {
                    Ok(guard) => Some(guard),
                    Err(_) => {
                        let rejected =
                            crate::i18n::get_required_cli_string("cli-fresh-confirmation-busy");
                        let outcome = record_rejected_tool_call(
                            ctx,
                            &tool_name,
                            &confirmation_args,
                            iteration,
                            rejected,
                            false,
                        )
                        .await;
                        if let Some(tx) = ctx.event_tx {
                            emit_tool_call_pair(tx, call, &outcome).await;
                        }
                        ordered_results[idx] =
                            Some((tool_name.clone(), call.tool_call_id.clone(), outcome));
                        continue;
                    }
                }
            } else {
                None
            };

        let requires_prompt = confirmation_requirement
            == zeroclaw_api::tool::ConfirmationRequirement::Fresh
            || ctx
                .approval
                .map(|mgr| mgr.needs_approval(&tool_name))
                .unwrap_or(false);
        let reentrant_agent_tool =
            crate::tools::REENTRANT_AGENT_TOOLS.contains(&tool_name.as_str());
        if requires_prompt && tool_name == "shell" && !reentrant_agent_tool {
            let prompt_signature = tool_call_signature(&tool_name, &tool_args);
            if !prompt_approval_tool_signatures_this_round.insert(prompt_signature.clone()) {
                let duplicate = record_rejected_tool_call(
                    ctx,
                    &tool_name,
                    &tool_args,
                    iteration,
                    format!(
                        "Skipped duplicate tool call '{tool_name}' with identical arguments in this turn."
                    ),
                    true,
                )
                .await;
                ordered_results[idx] =
                    Some((tool_name.clone(), call.tool_call_id.clone(), duplicate));
                continue;
            }
            if !prompt_approval_tool_signatures.insert(prompt_signature) {
                let repeated = format!(
                    "Agent loop aborted: repeated prompt-required tool call '{tool_name}' with identical arguments before approval."
                );
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "model": ctx.model,
                            "iteration": iteration + 1,
                            "tool": tool_name.clone(),
                            "arguments": scrub_credentials(&tool_args.to_string()),
                            "result": repeated,
                            "trace_id": ctx.turn_id,
                        })),
                    "tool_call_result"
                );
                if let Some(tx) = ctx.on_delta {
                    let _ = tx
                        .send(StreamDelta::Status(format!(
                            "\u{274c} {}: {}\n",
                            tool_name, repeated
                        )))
                        .await;
                }
                anyhow::bail!("{repeated}");
            }
        }

        // ── Approval hook ────────────────────────────────
        let approved = match gate_tool_approval(
            ctx,
            &tool_name,
            &confirmation_args,
            confirmation_requirement,
            iteration,
        )
        .await
        {
            ApprovalGateOutcome::Proceed { approved } => approved,
            ApprovalGateOutcome::Deny(outcome) | ApprovalGateOutcome::Replace(outcome) => {
                // Streaming consumers see the denied/replaced call and its
                // synthesized result (e.g. a DenyWithEdit replacement) as a
                // ToolCall/ToolResult pair, as the direct path always did.
                if let Some(tx) = ctx.event_tx {
                    emit_tool_call_pair(tx, call, &outcome).await;
                }
                ordered_results[idx] =
                    Some((tool_name.clone(), call.tool_call_id.clone(), outcome));
                continue;
            }
        };
        crate::agent::set_runtime_approved_arg(&tool_name, &mut tool_args, approved);
        crate::agent::set_fresh_confirmation_approved_arg(
            confirmation_requirement,
            &mut tool_args,
            approved,
        );
        let approval_context = approved.then(|| zeroclaw_api::tool::ApprovalContext {
            provenance: match confirmation_requirement {
                zeroclaw_api::tool::ConfirmationRequirement::Policy => {
                    zeroclaw_api::tool::ApprovalProvenance::Policy
                }
                zeroclaw_api::tool::ConfirmationRequirement::Fresh => {
                    zeroclaw_api::tool::ApprovalProvenance::Fresh
                }
            },
            effective_arguments: confirmation_args.clone(),
        });
        crate::agent::set_computer_use_approval_context(
            &tool_name,
            &mut tool_args,
            approval_context,
        );

        let signature = tool_call_signature(&tool_name, &tool_args);
        let dedup_exempt =
            ctx.dedup_exempt_tools.iter().any(|e| e == &tool_name) || reentrant_agent_tool;
        if dedup_enabled && !dedup_exempt && !seen_tool_signatures.insert(signature) {
            let duplicate = record_rejected_tool_call(
                ctx,
                &tool_name,
                &tool_args,
                iteration,
                format!(
                    "Skipped duplicate tool call '{tool_name}' with identical arguments in this turn."
                ),
                true,
            )
            .await;
            ordered_results[idx] = Some((tool_name.clone(), call.tool_call_id.clone(), duplicate));
            continue;
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
                .with_category(::zeroclaw_log::EventCategory::Tool)
                .with_attrs(::serde_json::json!({
                    "model": ctx.model,
                    "iteration": iteration + 1,
                    "tool": tool_name.clone(),
                    "arguments": scrub_credentials(&tool_args.to_string()),
                    "trace_id": ctx.turn_id,
                })),
            "tool_call_start"
        );

        // ── Progress: tool start ────────────────────────────
        if let Some(tx) = ctx.on_delta {
            let hint = {
                let raw = match tool_name.as_str() {
                    "shell" => tool_args.get("command").and_then(|v| v.as_str()),
                    "file_read" | "file_write" => tool_args.get("path").and_then(|v| v.as_str()),
                    _ => tool_args
                        .get("action")
                        .and_then(|v| v.as_str())
                        .or_else(|| tool_args.get("query").and_then(|v| v.as_str())),
                };
                match raw {
                    Some(s) => truncate_with_ellipsis(s, 60),
                    None => String::new(),
                }
            };
            let progress = if hint.is_empty() {
                format!("\u{23f3} {}\n", tool_name)
            } else {
                format!("\u{23f3} {}: {hint}\n", tool_name)
            };
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Tool)
                    .with_attrs(::serde_json::json!({"tool": tool_name})),
                "Sending progress start to draft"
            );
            let _ = tx.send(StreamDelta::Status(progress)).await;
        }

        executable_indices.push(idx);
        let call_id = super::events::resolve_tool_call_id(&ParsedToolCall {
            name: tool_name.clone(),
            arguments: tool_args.clone(),
            tool_call_id: call.tool_call_id.clone(),
        });
        // Pin the resolved id onto the executable call so the pending ToolCall
        // and the terminal ToolResult (both emitted by the executor at dispatch
        // and completion) share one correlation id, even for id-less
        // text-protocol calls.
        executable_calls.push(ParsedToolCall {
            name: tool_name,
            arguments: tool_args,
            tool_call_id: Some(call_id),
        });
        if call_fresh_confirmation_guard.is_some() {
            debug_assert!(fresh_confirmation_guard.is_none());
            fresh_confirmation_guard = call_fresh_confirmation_guard;
        }
    }

    Ok(PreparedToolCalls {
        ordered_results,
        executable_indices,
        executable_calls,
        fresh_confirmation_guard,
    })
}
