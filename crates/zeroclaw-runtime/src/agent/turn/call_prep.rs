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
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::attribution::Role;
use zeroclaw_tool_call_parser::{ParsedToolCall, canonicalize_json_for_tool_signature};

pub(crate) struct PreparedToolCalls {
    pub(crate) ordered_results: Vec<Option<(String, Option<String>, ToolExecutionOutcome)>>,
    pub(crate) executable_indices: Vec<usize>,
    pub(crate) executable_calls: Vec<ParsedToolCall>,
    /// Per-call immutable snapshot for draft start/completion events.
    pub(crate) stream_calls: Vec<Option<StreamToolCall>>,
}

/// Per-call draft metadata retained only until the matching completion event.
/// The arguments are the prepared call arguments; `tool_role` is copied from
/// the resolved tool's canonical attribution, not from a second registry.
pub(crate) struct StreamToolCall {
    pub(crate) arguments: Arc<serde_json::Value>,
    pub(crate) tool_role: Option<Role>,
}

fn tool_call_signature(tool_name: &str, tool_args: &serde_json::Value) -> (String, String) {
    let canonical_args = canonicalize_json_for_tool_signature(tool_args);
    let args_json = serde_json::to_string(&canonical_args).unwrap_or_else(|_| "{}".to_string());
    (tool_name.trim().to_ascii_lowercase(), args_json)
}

async fn record_duplicate_tool_call(
    ctx: &TurnCtx<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    iteration: usize,
) -> ToolExecutionOutcome {
    let duplicate =
        format!("Skipped duplicate tool call '{tool_name}' with identical arguments in this turn.");
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
                "result": duplicate,
                "deduplicated": true,
                "trace_id": ctx.turn_id,
            })),
        "tool_call_result"
    );
    if let Some(tx) = ctx.on_delta {
        let _ = tx
            .send(StreamDelta::Status(format!(
                "\u{274c} {}: {}\n",
                tool_name, duplicate
            )))
            .await;
    }
    ToolExecutionOutcome {
        output: duplicate.clone(),
        success: false,
        error_reason: Some(duplicate),
        duration: Duration::ZERO,
        receipt: None,
        output_data: None,
    }
}

/// Run per-call preparation over this round's parsed tool calls (upstream
/// loop body, per-call prep loop).
pub(crate) async fn prepare_tool_calls(
    ctx: &TurnCtx<'_>,
    tools_registry: &[Box<dyn crate::tools::Tool>],
    tool_calls: &[ParsedToolCall],
    seen_tool_signatures: &mut HashSet<(String, String)>,
    prompt_approval_tool_signatures: &mut HashSet<(String, String)>,
    iteration: usize,
    dedup_enabled: bool,
) -> Result<PreparedToolCalls> {
    let mut ordered_results: Vec<Option<(String, Option<String>, ToolExecutionOutcome)>> =
        (0..tool_calls.len()).map(|_| None).collect();
    let mut executable_indices: Vec<usize> = Vec::new();
    let mut executable_calls: Vec<ParsedToolCall> = Vec::new();
    let mut executable_stream_calls = Vec::new();
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

        let requires_prompt = ctx
            .approval
            .map(|mgr| mgr.needs_approval(&tool_name))
            .unwrap_or(false);
        let reentrant_agent_tool =
            crate::tools::REENTRANT_AGENT_TOOLS.contains(&tool_name.as_str());
        if requires_prompt && tool_name == "shell" && !reentrant_agent_tool {
            let prompt_signature = tool_call_signature(&tool_name, &tool_args);
            if !prompt_approval_tool_signatures_this_round.insert(prompt_signature.clone()) {
                let duplicate =
                    record_duplicate_tool_call(ctx, &tool_name, &tool_args, iteration).await;
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
        let approved = match gate_tool_approval(ctx, &tool_name, &tool_args, iteration).await {
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

        let signature = tool_call_signature(&tool_name, &tool_args);
        let dedup_exempt =
            ctx.dedup_exempt_tools.iter().any(|e| e == &tool_name) || reentrant_agent_tool;
        if dedup_enabled && !dedup_exempt && !seen_tool_signatures.insert(signature) {
            let duplicate =
                record_duplicate_tool_call(ctx, &tool_name, &tool_args, iteration).await;
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
        let stream_call = ctx.on_delta.map(|_| StreamToolCall {
            arguments: Arc::new(tool_args.clone()),
            tool_role: crate::agent::tool_execution::find_tool(tools_registry, &tool_name)
                .map(|tool| tool.role()),
        });
        if let (Some(tx), Some(stream_call)) = (ctx.on_delta, stream_call.as_ref()) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Tool)
                    .with_attrs(::serde_json::json!({"tool": tool_name})),
                "Sending progress start to draft"
            );
            let _ = tx
                .send(StreamDelta::ToolStart {
                    tool: tool_name.clone(),
                    arguments: Arc::clone(&stream_call.arguments),
                    tool_role: stream_call.tool_role,
                })
                .await;
        }

        executable_indices.push(idx);
        executable_stream_calls.push(stream_call);
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
    }

    Ok(PreparedToolCalls {
        ordered_results,
        executable_indices,
        executable_calls,
        stream_calls: executable_stream_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::{PreparedToolCalls, prepare_tool_calls};
    use crate::agent::tool_execution::ToolExecutionOutcome;
    use crate::agent::turn::context::TurnCtx;
    use crate::agent::turn::post_exec::record_executed_outcomes;
    use crate::agent::turn::{DraftEvent, StreamDelta};
    use crate::observability::NoopObserver;
    use crate::tools::{Tool, ToolResult};
    use async_trait::async_trait;
    use std::collections::HashSet;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
    use zeroclaw_config::schema::{PacingConfig, StreamReasoningMode};
    use zeroclaw_tool_call_parser::ParsedToolCall;

    struct AttributedTool {
        name: String,
        role: Role,
    }

    impl Attributable for AttributedTool {
        fn role(&self) -> Role {
            self.role
        }

        fn alias(&self) -> &str {
            &self.name
        }
    }

    #[async_trait]
    impl Tool for AttributedTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "test tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    fn test_ctx<'a>(
        observer: &'a NoopObserver,
        pacing: &'a PacingConfig,
        on_delta: &'a mpsc::Sender<DraftEvent>,
    ) -> TurnCtx<'a> {
        TurnCtx {
            observer,
            provider_name: "test",
            model: "test-model",
            temperature: None,
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: Some(on_delta),
            event_tx: None,
            hooks: None,
            dedup_exempt_tools: &[],
            pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "test-turn",
            agent_alias: None,
            parent_agent_alias: None,
        }
    }

    async fn emitted_tool_roles(
        tools_registry: Vec<Box<dyn Tool>>,
        tool_name: &str,
    ) -> Vec<Option<Role>> {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, mut rx) = mpsc::channel(4);
        let ctx = test_ctx(&observer, &pacing, &tx);
        let tool_calls = [ParsedToolCall {
            name: tool_name.to_string(),
            arguments: serde_json::json!({"action": "run"}),
            tool_call_id: Some("call-1".to_string()),
        }];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();
        let mut prepared: PreparedToolCalls = prepare_tool_calls(
            &ctx,
            &tools_registry,
            &tool_calls,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        )
        .await
        .expect("preparation should accept the test call");

        record_executed_outcomes(
            &ctx,
            &prepared.executable_indices,
            &prepared.executable_calls,
            &prepared.stream_calls,
            vec![ToolExecutionOutcome {
                output: "ok".to_string(),
                output_data: None,
                success: true,
                error_reason: None,
                duration: Duration::ZERO,
                receipt: None,
            }],
            &mut prepared.ordered_results,
            0,
        )
        .await;

        let mut roles = Vec::new();
        for _ in 0..2 {
            match rx.recv().await.expect("start and completion events") {
                StreamDelta::ToolStart { tool_role, .. }
                | StreamDelta::ToolComplete { tool_role, .. } => roles.push(tool_role),
                other => panic!("expected a tool event, got {other:?}"),
            }
        }
        roles
    }

    #[tokio::test]
    async fn prepare_carries_resolved_role_through_start_and_completion() {
        let wasm_role = Role::Tool(ToolKind::WasmPlugin);
        let wasm_registry: Vec<Box<dyn Tool>> = vec![Box::new(AttributedTool {
            name: "wasm-test".to_string(),
            role: wasm_role,
        })];
        assert_eq!(
            emitted_tool_roles(wasm_registry, "wasm-test").await,
            vec![Some(wasm_role), Some(wasm_role)],
            "the role resolved during preparation must be identical in both events"
        );

        assert_eq!(
            emitted_tool_roles(Vec::new(), "unresolved-test").await,
            vec![None, None],
            "an unresolved tool must remain untrusted through both events"
        );
    }
}
