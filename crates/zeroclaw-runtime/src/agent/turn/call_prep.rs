//! The per-call preparation loop: `before_tool_call` hook, delivery defaults,
//! the approval gate, the duplicate-call gate, and start logging — producing
//! the executable subset of this round's tool calls.

use super::approval_gate::{ApprovalGateOutcome, gate_tool_approval};
use super::context::TurnCtx;
use super::delivery_defaults::maybe_inject_channel_delivery_defaults;
use super::events::{ProgressEvent, StreamDelta, emit_tool_call_pair, send_progress};
use super::outcome::ToolLoopCancelled;
use super::redact::{loggable_args_string, loggable_args_value, scrub_credentials};
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::attribution::ToolProvenance;
use zeroclaw_api::hook::ToolCallHookContext;
use zeroclaw_tool_call_parser::{ParsedToolCall, canonicalize_json_for_tool_signature};

pub(crate) struct PreparedToolCalls {
    pub(crate) ordered_results: Vec<Option<(String, Option<String>, ToolExecutionOutcome)>>,
    pub(crate) executable_indices: Vec<usize>,
    pub(crate) executable_calls: Vec<ParsedToolCall>,
    /// Per-call immutable snapshot for draft start/completion events.
    pub(crate) stream_calls: Vec<Option<StreamToolCall>>,
}

/// Per-call draft metadata retained only until the matching completion event.
/// The arguments are the prepared call arguments; `tool_provenance` is copied
/// from the resolved tool's canonical attribution, not from a second registry.
pub(crate) struct StreamToolCall {
    pub(crate) arguments: Arc<serde_json::Value>,
    /// A narrow carry-through while draft events carry presentation metadata,
    /// not resolved tool identity. A future event-subsystem redesign should
    /// carry the resolved tool identity directly instead of copying its
    /// provenance into paired events.
    pub(crate) tool_provenance: Option<ToolProvenance>,
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
                "arguments": loggable_args_string(ctx.tool_by_name(tool_name), tool_args),
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

/// Fire the abandonment callback for one call whose before phase ran but that
/// can no longer reach its after phase (hook-cancelled, denied/replaced, or
/// duplicate-suppressed during preparation). Abandonment is this call's single
/// terminal lifecycle operation; it must never be paired with an after hook.
async fn abandon_prepared_context(ctx: &TurnCtx<'_>, context: &ToolCallHookContext, tool: &str) {
    if let Some(hooks) = ctx.hooks {
        hooks.fire_tool_call_abandoned(context, tool).await;
    }
}

/// Fire the abandonment callback for every executable call position of a
/// preparation batch that never reached post-execution handling.
///
/// `completed` holds the positions whose outcomes were recorded (their after
/// hook already ran — a completed call must not be abandoned). Called by the
/// execution phase when the batch aborts: non-cancel execution errors and
/// mid-batch interruption both leave executable contexts without a terminal
/// operation.
pub(crate) async fn abandon_unexecuted_prepared_contexts(
    ctx: &TurnCtx<'_>,
    iteration: usize,
    executable_indices: &[usize],
    executable_calls: &[ParsedToolCall],
    completed: &[usize],
) {
    let Some(hooks) = ctx.hooks else {
        return;
    };
    for (call_idx, call) in executable_indices.iter().zip(executable_calls.iter()) {
        if completed.contains(call_idx) {
            continue;
        }
        let context = crate::hooks::tool_call_hook_context(ctx.turn_id, iteration, *call_idx);
        hooks.fire_tool_call_abandoned(&context, &call.name).await;
    }
}

/// Emit a synthetic pre-execution event pair from the effective call the host
/// actually reviewed. The post-hook name and arguments are projected through
/// the resolved tool's presentation boundary; unresolved names expose no
/// arguments. The model-provided call ID is retained only for correlation.
async fn emit_synthetic_tool_call_pair(
    ctx: &TurnCtx<'_>,
    original_call: &ParsedToolCall,
    tool_name: &str,
    tool_args: &serde_json::Value,
    outcome: &ToolExecutionOutcome,
) {
    let Some(tx) = ctx.event_tx else {
        return;
    };
    let event_call = ParsedToolCall {
        name: tool_name.to_string(),
        arguments: ctx
            .tool_by_name(tool_name)
            .map(|tool| loggable_args_value(Some(tool), tool_args))
            .unwrap_or_else(|| serde_json::json!({})),
        tool_call_id: original_call.tool_call_id.clone(),
    };
    emit_tool_call_pair(tx, &event_call, outcome).await;
}

/// Run per-call preparation over this round's parsed tool calls (upstream
/// loop body, per-call prep loop).
pub(crate) async fn prepare_tool_calls(
    ctx: &TurnCtx<'_>,
    tools_registry: &[Box<dyn crate::tools::Tool>],
    activated_tools: Option<&Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
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
    // Contexts whose before phase ran and that are still awaiting a terminal
    // lifecycle operation (before hook continued, no cancel/deny/dedup yet).
    // They become the execution phase's responsibility on success; on a
    // preparation abort they are abandoned here so none leak. Each carries the
    // prepared tool name it would have executed under.
    let mut retained_hook_contexts: Vec<(ToolCallHookContext, String)> = Vec::new();

    for (idx, call) in tool_calls.iter().enumerate() {
        // ── Hook: before_tool_call (modifying) ──────────
        let mut tool_name = call.name.clone();
        let mut tool_args = call.arguments.clone();
        let hook_context = crate::hooks::tool_call_hook_context(ctx.turn_id, iteration, idx);
        if let Some(hooks) = ctx.hooks {
            match hooks
                .run_before_tool_call_with_context(
                    &hook_context,
                    tool_name.clone(),
                    tool_args.clone(),
                )
                .await
            {
                crate::hooks::HookResult::Cancel(reason) => {
                    // The before phase ran, so this context's only terminal
                    // lifecycle operation is abandonment.
                    abandon_prepared_context(ctx, &hook_context, &tool_name).await;
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
                                "arguments": loggable_args_string(ctx.tool_by_name(&tool_name), &tool_args),
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
                    emit_synthetic_tool_call_pair(ctx, call, &tool_name, &tool_args, &outcome)
                        .await;
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
            ctx.channel
                .map(zeroclaw_api::attribution::Attributable::alias),
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
                abandon_prepared_context(ctx, &hook_context, &tool_name).await;
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
                            "arguments": loggable_args_string(ctx.tool_by_name(&tool_name), &tool_args),
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
                // Preparation aborts after earlier calls in this batch were
                // retained: none of them (nor this call) can reach execution,
                // so each gets exactly one abandonment.
                for (retained_context, retained_tool) in &retained_hook_contexts {
                    abandon_prepared_context(ctx, retained_context, retained_tool).await;
                }
                abandon_prepared_context(ctx, &hook_context, &tool_name).await;
                anyhow::bail!("{repeated}");
            }
        }

        // ── Approval hook ────────────────────────────────
        // The batch position comes from this enumeration, so the card can say
        // which of the model's calls it is describing. It counts every call in
        // the batch, not just the ones that need approval.
        let position = zeroclaw_api::channel::ApprovalPosition {
            index: u32::try_from(idx + 1).unwrap_or(u32::MAX),
            total: u32::try_from(tool_calls.len()).unwrap_or(u32::MAX),
        };
        let approved =
            match gate_tool_approval(ctx, &tool_name, &mut tool_args, iteration, position).await {
                ApprovalGateOutcome::Proceed { approved } => approved,
                ApprovalGateOutcome::Deny(outcome) | ApprovalGateOutcome::Replace(outcome) => {
                    // The before phase ran but this call will never execute:
                    // its only terminal lifecycle operation is abandonment.
                    abandon_prepared_context(ctx, &hook_context, &tool_name).await;
                    // Streaming consumers see the denied/replaced call and its
                    // synthesized result (e.g. a DenyWithEdit replacement) as a
                    // ToolCall/ToolResult pair, as the direct path always did.
                    emit_synthetic_tool_call_pair(ctx, call, &tool_name, &tool_args, &outcome)
                        .await;
                    ordered_results[idx] =
                        Some((tool_name.clone(), call.tool_call_id.clone(), outcome));
                    continue;
                }
                ApprovalGateOutcome::Cancelled => {
                    // Preparation aborts before execution takes ownership of
                    // the retained contexts or the call awaiting approval.
                    for (retained_context, retained_tool) in &retained_hook_contexts {
                        abandon_prepared_context(ctx, retained_context, retained_tool).await;
                    }
                    abandon_prepared_context(ctx, &hook_context, &tool_name).await;
                    return Err(ToolLoopCancelled.into());
                }
            };
        let runtime_command_approved = approved
            && ctx
                .approval
                .is_none_or(crate::approval::ApprovalManager::propagates_runtime_command_approval);
        crate::agent::set_runtime_approved_arg(
            &tool_name,
            &mut tool_args,
            runtime_command_approved,
        );

        let signature = tool_call_signature(&tool_name, &tool_args);
        let dedup_exempt =
            ctx.dedup_exempt_tools.iter().any(|e| e == &tool_name) || reentrant_agent_tool;
        if dedup_enabled && !dedup_exempt && !seen_tool_signatures.insert(signature) {
            let duplicate =
                record_duplicate_tool_call(ctx, &tool_name, &tool_args, iteration).await;
            abandon_prepared_context(ctx, &hook_context, &tool_name).await;
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
                    "arguments": loggable_args_string(ctx.tool_by_name(&tool_name), &tool_args),
                    "trace_id": ctx.turn_id,
                })),
            "tool_call_start"
        );

        // ── Progress: tool start ────────────────────────────
        send_progress(ctx.on_delta, ProgressEvent::RunningTool).await;
        let stream_call = ctx.on_delta.map(|_| StreamToolCall {
            arguments: Arc::new(loggable_args_value(
                ctx.tool_by_name(&tool_name),
                &tool_args,
            )),
            tool_provenance: crate::agent::tool_execution::resolved_tool_provenance(
                tools_registry,
                activated_tools,
                &tool_name,
            ),
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
                    tool_provenance: stream_call.tool_provenance,
                })
                .await;
        }

        executable_indices.push(idx);
        executable_stream_calls.push(stream_call);
        // From here the context's terminal operation is execution's
        // responsibility: the after hook on completion, or abandonment if the
        // execution phase aborts before post-execution handling.
        retained_hook_contexts.push((hook_context, tool_name.clone()));
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
    use crate::approval::ApprovalManager;
    use crate::hooks::{HookHandler, HookResult, HookRunner};
    use crate::observability::NoopObserver;
    use crate::skills::SkillTool;
    use crate::tools::config_patch::ConfigPatchTool;
    use crate::tools::skill_tool::SkillBuiltinTool;
    use crate::tools::{Tool, ToolResult};
    use async_trait::async_trait;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use zeroclaw_api::agent::TurnEvent;
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role, ToolProvenance};
    use zeroclaw_api::channel::{
        Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
    };
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::{Config, PacingConfig, RiskProfileConfig, StreamReasoningMode};
    use zeroclaw_tool_call_parser::ParsedToolCall;

    struct AttributedTool {
        name: String,
        provenance: ToolProvenance,
        redact_args: bool,
    }

    impl Attributable for AttributedTool {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
        }

        fn alias(&self) -> &str {
            &self.name
        }

        fn tool_provenance(&self) -> ToolProvenance {
            self.provenance
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

        fn redact_args_for_log(&self, _args: &serde_json::Value) -> Option<serde_json::Value> {
            self.redact_args
                .then(|| serde_json::json!({"value": "[redacted]"}))
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    struct ApprovingChatChannel {
        asked: AtomicUsize,
        replacement: bool,
    }

    impl Attributable for ApprovingChatChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Cli)
        }

        fn alias(&self) -> &str {
            "approving-chat"
        }
    }

    #[async_trait]
    impl Channel for ApprovingChatChannel {
        fn name(&self) -> &str {
            "approving-chat"
        }

        fn is_operator_approval_surface(&self) -> bool {
            self.replacement
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn request_approval(
            &self,
            _recipient: &str,
            _request: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            Ok(Some(if self.replacement {
                ChannelApprovalResponse::DenyWithEdit {
                    replacement: "safe replacement".into(),
                }
            } else {
                ChannelApprovalResponse::Approve
            }))
        }
    }

    struct RewriteConfigPatchHook {
        arguments: serde_json::Value,
    }

    #[async_trait]
    impl HookHandler for RewriteConfigPatchHook {
        fn name(&self) -> &str {
            "rewrite-config-patch"
        }

        async fn before_tool_call(
            &self,
            _name: String,
            _args: serde_json::Value,
        ) -> HookResult<(String, serde_json::Value)> {
            HookResult::Continue(("config_patch".into(), self.arguments.clone()))
        }
    }

    struct CancelConfigPatchHook;

    #[async_trait]
    impl HookHandler for CancelConfigPatchHook {
        fn name(&self) -> &str {
            "cancel-config-patch"
        }

        async fn before_tool_call(
            &self,
            _name: String,
            _args: serde_json::Value,
        ) -> HookResult<(String, serde_json::Value)> {
            HookResult::Cancel("blocked by test hook".to_string())
        }
    }

    fn config_patch_args(path: &str, value: &str) -> serde_json::Value {
        serde_json::json!({
            "ops": [{
                "op": "add",
                "path": path,
                "value": value,
                "comment": value
            }]
        })
    }

    async fn assert_synthetic_pair_is_redacted(
        rx: &mut mpsc::Receiver<TurnEvent>,
        sentinel: &str,
        expected_path: &str,
    ) {
        let pending = rx.recv().await.expect("synthetic ToolCall event");
        let result = rx.recv().await.expect("synthetic ToolResult event");
        let (pending_id, rendered) = match pending {
            TurnEvent::ToolCall { id, name, args } => {
                assert_eq!(name, "config_patch");
                assert_eq!(args["ops"][0]["path"], expected_path);
                (id, args.to_string())
            }
            other => panic!("expected ToolCall first, got {other:?}"),
        };
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(!rendered.contains(sentinel), "{rendered}");
        match result {
            TurnEvent::ToolResult { id, name, .. } => {
                assert_eq!(name, "config_patch");
                assert_eq!(id, pending_id);
            }
            other => panic!("expected ToolResult second, got {other:?}"),
        }
    }

    /// The routed view of a turn context is what the tool phase receives, so
    /// it must carry the registry the approval gate and argument redaction
    /// resolve tools against. Dropping it would silently disable operator-only
    /// gating and secret redaction on every routed turn.
    #[test]
    fn route_specific_context_keeps_the_tool_registry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ConfigPatchTool::new(
            dir.path().join("config.toml"),
            Arc::new(SecurityPolicy::default()),
        ))];
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (on_delta, _on_delta_rx) = mpsc::channel(1);
        let mut base = test_ctx(&observer, &pacing, &on_delta);
        base.tools = &tools;

        let routed = base.for_route(
            "routed-provider",
            "routed-model",
            zeroclaw_config::schema::ResolvedContextLimits::legacy_fallback(0),
        );

        let tool = routed
            .tool_by_name("config_patch")
            .expect("a routed context must resolve registry tools");
        assert!(
            tool.approval_requires_operator(),
            "the routed view must keep the operator-only marker the gate enforces"
        );
        assert_eq!(routed.tools.len(), base.tools.len());
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
            context_limits: zeroclaw_config::schema::ResolvedContextLimits::legacy_fallback(0),
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
            serving_provider_name: None,
            serving_model: None,
            tools: &[],
        }
    }

    async fn emitted_tool_provenance(
        tools_registry: Vec<Box<dyn Tool>>,
        tool_name: &str,
    ) -> Vec<Option<ToolProvenance>> {
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
            None,
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

        let mut provenance = Vec::new();
        while provenance.len() < 2 {
            match rx.recv().await.expect("start and completion events") {
                StreamDelta::ToolStart {
                    tool_provenance, ..
                }
                | StreamDelta::ToolComplete {
                    tool_provenance, ..
                } => provenance.push(tool_provenance),
                StreamDelta::Lifecycle(_) => {}
                other => panic!("expected a tool event, got {other:?}"),
            }
        }
        provenance
    }

    #[tokio::test]
    async fn prepare_streams_the_tools_secret_aware_argument_projection() {
        let sentinel = "sentinel-stream-secret-must-not-leak";
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(AttributedTool {
            name: "secret-test".to_string(),
            provenance: ToolProvenance::Extension,
            redact_args: true,
        })];
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, mut rx) = mpsc::channel(2);
        let ctx = TurnCtx {
            tools: &tools,
            ..test_ctx(&observer, &pacing, &tx)
        };
        let tool_calls = [ParsedToolCall {
            name: "secret-test".to_string(),
            arguments: serde_json::json!({"value": sentinel}),
            tool_call_id: Some("call-secret".to_string()),
        }];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();

        prepare_tool_calls(
            &ctx,
            &tools,
            None,
            &tool_calls,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        )
        .await
        .expect("preparation should accept the test call");

        loop {
            match rx.recv().await.expect("tool start event") {
                StreamDelta::ToolStart { arguments, .. } => {
                    let rendered = arguments.to_string();
                    assert!(rendered.contains("[redacted]"), "{rendered}");
                    assert!(!rendered.contains(sentinel), "{rendered}");
                    break;
                }
                StreamDelta::Lifecycle(_) => {}
                other => panic!("expected a tool start event, got {other:?}"),
            }
        }
    }

    async fn exercise_config_patch_synthetic_event(
        full: bool,
        replacement: bool,
        unknown_fields: bool,
        with_manager: bool,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        Config {
            config_path: config_path.clone(),
            ..Config::default()
        }
        .save()
        .await
        .expect("seed config");
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ConfigPatchTool::new(
            config_path.clone(),
            Arc::new(SecurityPolicy::default()),
        ))];
        let sentinel = "sentinel-denied-event-secret-01234567";
        let rewritten_path = "/http_request/secrets/api_token";
        let mut hooks = HookRunner::new();
        let mut args = config_patch_args(rewritten_path, sentinel);
        if unknown_fields {
            args["extra"] = serde_json::json!(sentinel);
            args["ops"][0]["extra"] = serde_json::json!(sentinel);
        }
        hooks.register(Box::new(RewriteConfigPatchHook { arguments: args }));
        let profile = RiskProfileConfig {
            level: if full {
                zeroclaw_config::autonomy::AutonomyLevel::Full
            } else {
                zeroclaw_config::autonomy::AutonomyLevel::Supervised
            },
            auto_approve: vec!["config_patch".to_string()],
            always_ask: vec![],
            ..RiskProfileConfig::default()
        };
        let approval = ApprovalManager::for_non_interactive(&profile);
        let channel = ApprovingChatChannel {
            asked: AtomicUsize::new(0),
            replacement,
        };
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (event_tx, mut event_rx) = mpsc::channel(4);
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            context_limits: zeroclaw_config::schema::ResolvedContextLimits::legacy_fallback(0),
            temperature: None,
            approval: with_manager.then_some(&approval),
            channel_name: "approving-chat",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: Some(&event_tx),
            hooks: Some(&hooks),
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: Some(&channel),
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "denied-event-redaction",
            agent_alias: None,
            parent_agent_alias: None,
            tools: &tools,
            serving_provider_name: None,
            serving_model: None,
        };
        let calls = [ParsedToolCall {
            name: "original_model_name".to_string(),
            arguments: config_patch_args("/gateway/host", "original-model-value"),
            tool_call_id: Some("denied-call".to_string()),
        }];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();

        let prepared = prepare_tool_calls(
            &ctx,
            &tools,
            None,
            &calls,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        )
        .await
        .expect("denial is a prepared synthetic result");

        assert!(prepared.executable_calls.is_empty());
        assert!(prepared.ordered_results[0].is_some());
        assert_eq!(
            channel.asked.load(Ordering::SeqCst),
            usize::from(replacement),
            "only the operator surface receives the prompt"
        );
        let outcome = &prepared.ordered_results[0].as_ref().unwrap().2;
        assert_eq!(outcome.success, replacement);
        if replacement {
            assert_eq!(outcome.output, "safe replacement");
        }
        let saved: Config =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(saved.http_request.secrets.is_empty());
        assert_synthetic_pair_is_redacted(&mut event_rx, sentinel, rewritten_path).await;
    }

    #[tokio::test]
    async fn denied_config_patch_emits_only_the_redacted_post_hook_call() {
        exercise_config_patch_synthetic_event(false, false, false, true).await;
    }
    #[tokio::test]
    async fn full_profile_cannot_bypass_operator_only_approval() {
        exercise_config_patch_synthetic_event(true, false, false, true).await;
    }
    #[tokio::test]
    async fn absent_manager_cannot_bypass_operator_only_approval() {
        exercise_config_patch_synthetic_event(true, false, false, false).await;
    }
    #[tokio::test]
    async fn replaced_config_patch_emits_only_the_redacted_post_hook_call() {
        exercise_config_patch_synthetic_event(true, true, false, true).await;
    }
    #[tokio::test]
    async fn invalid_unknown_patch_fields_never_reach_synthetic_events() {
        exercise_config_patch_synthetic_event(false, false, true, true).await;
    }

    #[tokio::test]
    async fn hook_cancelled_config_patch_uses_the_shared_redacted_pair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ConfigPatchTool::new(
            dir.path().join("config.toml"),
            Arc::new(SecurityPolicy::default()),
        ))];
        let sentinel = "sentinel-cancelled-event-secret-01234567";
        let secret_path = "/http_request/secrets/api_token";
        let mut hooks = HookRunner::new();
        hooks.register(Box::new(CancelConfigPatchHook));
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (event_tx, mut event_rx) = mpsc::channel(4);
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            context_limits: zeroclaw_config::schema::ResolvedContextLimits::legacy_fallback(0),
            temperature: None,
            approval: None,
            channel_name: "test",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: Some(&event_tx),
            hooks: Some(&hooks),
            dedup_exempt_tools: &[],
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            draft_reasoning: StreamReasoningMode::Status,
            turn_id: "cancelled-event-redaction",
            agent_alias: None,
            parent_agent_alias: None,
            tools: &tools,
            serving_provider_name: None,
            serving_model: None,
        };
        let calls = [ParsedToolCall {
            name: "config_patch".to_string(),
            arguments: {
                let mut args = config_patch_args(secret_path, sentinel);
                args["extra"] = serde_json::json!(sentinel);
                args["ops"][0]["extra"] = serde_json::json!(sentinel);
                args
            },
            tool_call_id: Some("cancelled-call".to_string()),
        }];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();

        let prepared = prepare_tool_calls(
            &ctx,
            &tools,
            None,
            &calls,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        )
        .await
        .expect("hook cancellation is a prepared synthetic result");

        assert!(prepared.executable_calls.is_empty());
        assert!(prepared.ordered_results[0].is_some());
        assert_synthetic_pair_is_redacted(&mut event_rx, sentinel, secret_path).await;
    }

    #[tokio::test]
    async fn prepare_carries_provenance_through_start_and_completion() {
        let extension_registry: Vec<Box<dyn Tool>> = vec![Box::new(AttributedTool {
            name: "extension-test".to_string(),
            provenance: ToolProvenance::Extension,
            redact_args: false,
        })];
        assert_eq!(
            emitted_tool_provenance(extension_registry, "extension-test").await,
            vec![
                Some(ToolProvenance::Extension),
                Some(ToolProvenance::Extension)
            ],
            "the provenance resolved during preparation must be identical in both events"
        );

        assert_eq!(
            emitted_tool_provenance(Vec::new(), "unresolved-test").await,
            vec![None, None],
            "an unresolved tool must remain untrusted through both events"
        );
    }

    #[tokio::test]
    async fn prepare_keeps_native_targets_wrapped_by_skills_as_extensions() {
        let target: Arc<dyn Tool> = Arc::new(AttributedTool {
            name: "browser".to_string(),
            provenance: ToolProvenance::Native,
            redact_args: false,
        });
        let skill_tool = SkillBuiltinTool::new(
            "skill_browser",
            &SkillTool {
                name: "open".to_string(),
                description: "Open a browser page through a skill".to_string(),
                kind: "builtin".to_string(),
                command: String::new(),
                args: HashMap::new(),
                target: Some("browser".to_string()),
                locked_args: HashMap::new(),
                timeout_secs: None,
            },
            target,
            HashMap::new(),
        );
        let skill_name = skill_tool.name().to_string();

        assert_eq!(
            emitted_tool_provenance(vec![Box::new(skill_tool)], &skill_name).await,
            vec![
                Some(ToolProvenance::Extension),
                Some(ToolProvenance::Extension)
            ],
            "the callable skill boundary, not its native target, controls both stream events"
        );
    }

    // ── Tool-call lifecycle abandonment (preparation paths) ──────────────

    struct LifecycleRecorder {
        events: Arc<std::sync::Mutex<Vec<String>>>,
        cancel_before_for: Vec<String>,
    }

    #[async_trait]
    impl crate::hooks::HookHandler for LifecycleRecorder {
        fn name(&self) -> &str {
            "lifecycle-recorder"
        }

        async fn before_tool_call_with_context(
            &self,
            context: &zeroclaw_api::hook::ToolCallHookContext,
            name: String,
            args: serde_json::Value,
        ) -> crate::hooks::HookResult<(String, serde_json::Value)> {
            self.events.lock().unwrap().push(format!(
                "before:{}:{}",
                name,
                context.invocation_id()
            ));
            if self.cancel_before_for.iter().any(|tool| tool == &name) {
                return crate::hooks::HookResult::Cancel("blocked by test".to_string());
            }
            crate::hooks::HookResult::Continue((name, args))
        }

        async fn on_after_tool_call_with_context(
            &self,
            context: &zeroclaw_api::hook::ToolCallHookContext,
            tool: &str,
            _result: &zeroclaw_api::tool::ToolResult,
            _duration: Duration,
        ) {
            self.events
                .lock()
                .unwrap()
                .push(format!("after:{}:{}", tool, context.invocation_id()));
        }

        async fn on_tool_call_abandoned(
            &self,
            context: &zeroclaw_api::hook::ToolCallHookContext,
            tool: &str,
        ) {
            self.events.lock().unwrap().push(format!(
                "abandoned:{}:{}",
                tool,
                context.invocation_id()
            ));
        }
    }

    fn lifecycle_ctx<'a>(
        observer: &'a NoopObserver,
        pacing: &'a PacingConfig,
        on_delta: &'a mpsc::Sender<DraftEvent>,
        approval: Option<&'a crate::approval::ApprovalManager>,
        hooks: Option<&'a crate::hooks::HookRunner>,
    ) -> TurnCtx<'a> {
        TurnCtx {
            approval,
            hooks,
            ..test_ctx(observer, pacing, on_delta)
        }
    }

    fn parsed_call(name: &str, arguments: serde_json::Value, tool_call_id: &str) -> ParsedToolCall {
        ParsedToolCall {
            name: name.to_string(),
            arguments,
            tool_call_id: Some(tool_call_id.to_string()),
        }
    }

    #[tokio::test]
    async fn hook_cancelled_call_reaches_abandonment_not_after() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, _rx) = mpsc::channel(8);
        let mut runner = crate::hooks::HookRunner::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        runner.register(Box::new(LifecycleRecorder {
            events: Arc::clone(&events),
            cancel_before_for: vec!["guarded".to_string()],
        }));
        let ctx = lifecycle_ctx(&observer, &pacing, &tx, None, Some(&runner));

        let tool_calls = [parsed_call(
            "guarded",
            serde_json::json!({"action": "run"}),
            "call-1",
        )];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();
        let prepared = prepare_tool_calls(
            &ctx,
            &[],
            None,
            &tool_calls,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        )
        .await
        .expect("preparation completes with the cancelled call recorded");

        let (_, _, outcome) = prepared.ordered_results[0]
            .as_ref()
            .expect("cancelled call records an outcome");
        assert!(!outcome.success, "hook-cancelled call must not execute");
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "before:guarded:test-turn:0:0".to_string(),
                "abandoned:guarded:test-turn:0:0".to_string(),
            ],
            "the cancelled context gets exactly one abandonment and never an after hook"
        );
    }

    #[tokio::test]
    async fn duplicate_suppression_abandons_only_the_suppressed_call() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, _rx) = mpsc::channel(8);
        let mut runner = crate::hooks::HookRunner::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        runner.register(Box::new(LifecycleRecorder {
            events: Arc::clone(&events),
            cancel_before_for: Vec::new(),
        }));
        let ctx = lifecycle_ctx(&observer, &pacing, &tx, None, Some(&runner));

        let tool_calls = [
            parsed_call("dup_tool", serde_json::json!({"action": "run"}), "call-a"),
            parsed_call("dup_tool", serde_json::json!({"action": "run"}), "call-b"),
        ];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();
        let mut prepared = prepare_tool_calls(
            &ctx,
            &[],
            None,
            &tool_calls,
            &mut seen,
            &mut prompt_seen,
            0,
            true,
        )
        .await
        .expect("preparation succeeds with the duplicate suppressed");

        assert_eq!(prepared.executable_indices, vec![0]);
        let (_, _, duplicate) = prepared.ordered_results[1]
            .as_ref()
            .expect("duplicate call records an outcome");
        assert!(!duplicate.success);
        assert!(duplicate.output.contains("duplicate"));

        // The surviving call completes normally through the after hook.
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

        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "before:dup_tool:test-turn:0:0".to_string(),
                "before:dup_tool:test-turn:0:1".to_string(),
                "abandoned:dup_tool:test-turn:0:1".to_string(),
                "after:dup_tool:test-turn:0:0".to_string(),
            ],
            "the duplicate is abandoned once; the surviving call correlates to its after hook"
        );
    }

    #[tokio::test]
    async fn approval_cancellation_abandons_retained_and_waiting_calls_once() {
        use crate::rpc::approval_channel::RpcApprovalChannel;
        use crate::rpc::context::ApprovalPendingMap;
        use tokio_util::sync::CancellationToken;
        use zeroclaw_api::jsonrpc::RpcOutbound;

        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, _rx) = mpsc::channel(8);
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut runner = crate::hooks::HookRunner::new();
        runner.register(Box::new(LifecycleRecorder {
            events: Arc::clone(&events),
            cancel_before_for: Vec::new(),
        }));
        let profile = zeroclaw_config::schema::RiskProfileConfig {
            auto_approve: vec!["normal_tool".to_string()],
            always_ask: vec!["guarded_tool".to_string()],
            ..Default::default()
        };
        let approval = crate::approval::ApprovalManager::for_non_interactive_backchannel(&profile);
        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(4);
        let channel = RpcApprovalChannel::new(
            "rpc",
            "session-approval",
            Arc::new(RpcOutbound::new(writer_tx)),
            Arc::new(ApprovalPendingMap::default()),
            Default::default(),
        );
        let cancel = CancellationToken::new();
        let ctx = TurnCtx {
            channel: Some(&channel),
            channel_reply_target: Some("operator"),
            cancellation_token: Some(&cancel),
            on_delta: None,
            ..lifecycle_ctx(&observer, &pacing, &tx, Some(&approval), Some(&runner))
        };
        let calls = [
            parsed_call("normal_tool", serde_json::json!({"n": 1}), "call-1"),
            parsed_call("guarded_tool", serde_json::json!({"n": 2}), "call-2"),
            parsed_call("unvisited_tool", serde_json::json!({"n": 3}), "call-3"),
        ];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();
        let preparation = prepare_tool_calls(
            &ctx,
            &[],
            None,
            &calls,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        );
        tokio::pin!(preparation);

        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                _ = &mut preparation => panic!("preparation must wait for approval"),
                request = writer_rx.recv() => {
                    request.expect("approval request must reach the RPC channel");
                }
            }
            cancel.cancel();
            let Err(error) = preparation.await else {
                panic!("cancelled approval must abort preparation");
            };
            assert!(error.is::<super::ToolLoopCancelled>());
        })
        .await
        .expect("approval cancellation must finish promptly");

        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "before:normal_tool:test-turn:0:0".to_string(),
                "before:guarded_tool:test-turn:0:1".to_string(),
                "abandoned:normal_tool:test-turn:0:0".to_string(),
                "abandoned:guarded_tool:test-turn:0:1".to_string(),
            ],
            "both prepared contexts must be abandoned once, with no after hooks or unvisited call"
        );
    }

    #[tokio::test]
    async fn approval_denied_call_is_abandoned() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, _rx) = mpsc::channel(8);
        let mut runner = crate::hooks::HookRunner::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        runner.register(Box::new(LifecycleRecorder {
            events: Arc::clone(&events),
            cancel_before_for: Vec::new(),
        }));
        let risk_profile = zeroclaw_config::schema::RiskProfileConfig {
            always_ask: vec!["guarded_tool".to_string()],
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let approval = crate::approval::ApprovalManager::for_non_interactive(&risk_profile);
        let ctx = lifecycle_ctx(&observer, &pacing, &tx, Some(&approval), Some(&runner));

        let tool_calls = [parsed_call(
            "guarded_tool",
            serde_json::json!({"action": "run"}),
            "call-1",
        )];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();
        let prepared = prepare_tool_calls(
            &ctx,
            &[],
            None,
            &tool_calls,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        )
        .await
        .expect("preparation completes with the denied call recorded");

        let (_, _, outcome) = prepared.ordered_results[0]
            .as_ref()
            .expect("denied call records an outcome");
        assert!(!outcome.success);
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "before:guarded_tool:test-turn:0:0".to_string(),
                "abandoned:guarded_tool:test-turn:0:0".to_string(),
            ],
            "a denial that prevents execution abandons the context instead of leaving it pending"
        );
    }

    #[tokio::test]
    async fn bounded_tool_approval_never_becomes_runtime_command_approval() {
        use zeroclaw_config::autonomy::AutonomyLevel;
        use zeroclaw_config::schema::RiskProfileConfig;

        let full_profile = RiskProfileConfig {
            level: AutonomyLevel::Full,
            ..RiskProfileConfig::default()
        };
        let bounded = crate::approval::ApprovalManager::for_bounded_non_interactive(&full_profile);
        let explicit_profile = RiskProfileConfig {
            auto_approve: vec!["*".to_string()],
            ..RiskProfileConfig::default()
        };
        let derived = bounded.derive_for_risk_profile(&explicit_profile);

        for (case, approval) in [("full", &bounded), ("auto_approve", &derived)] {
            for tool_name in ["shell", "schedule", "cron_add", "cron_update", "cron_run"] {
                let observer = NoopObserver;
                let pacing = PacingConfig::default();
                let (tx, _rx) = mpsc::channel(8);
                let ctx = lifecycle_ctx(&observer, &pacing, &tx, Some(approval), None);
                let tool_calls = [parsed_call(
                    tool_name,
                    serde_json::json!({"approved": true}),
                    "call-1",
                )];
                let mut seen = HashSet::new();
                let mut prompt_seen = HashSet::new();
                let prepared = prepare_tool_calls(
                    &ctx,
                    &[],
                    None,
                    &tool_calls,
                    &mut seen,
                    &mut prompt_seen,
                    0,
                    false,
                )
                .await
                .unwrap_or_else(|error| panic!("{case}/{tool_name} preparation failed: {error}"));

                assert_eq!(prepared.executable_calls.len(), 1, "{case}/{tool_name}");
                assert_eq!(
                    prepared.executable_calls[0].arguments["approved"], false,
                    "{case}/{tool_name} target policy may approve tool dispatch but not the caller-owned command"
                );
            }
        }
    }

    /// Channel that answers every approval request with a DenyWithEdit
    /// replacement, exercising the Replace arm of the approval gate.
    struct ReplacingChannel;

    impl zeroclaw_api::attribution::Attributable for ReplacingChannel {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Channel(
                zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "replacing-test"
        }
    }

    #[async_trait]
    impl zeroclaw_api::channel::Channel for ReplacingChannel {
        fn name(&self) -> &str {
            "replacing-test"
        }

        async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            Ok(())
        }

        async fn request_approval_attributed(
            &self,
            _recipient: &str,
            _request: &zeroclaw_api::channel::ChannelApprovalRequest,
        ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
            Ok(Some(
                zeroclaw_api::channel::AttributedApprovalResponse::operator(
                    zeroclaw_api::channel::ChannelApprovalResponse::DenyWithEdit {
                        replacement: "safe replacement".to_string(),
                    },
                )
                .with_decider("tester"),
            ))
        }
    }

    #[tokio::test]
    async fn approval_replaced_call_is_abandoned() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, _rx) = mpsc::channel(8);
        let mut runner = crate::hooks::HookRunner::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        runner.register(Box::new(LifecycleRecorder {
            events: Arc::clone(&events),
            cancel_before_for: Vec::new(),
        }));
        let risk_profile = zeroclaw_config::schema::RiskProfileConfig {
            always_ask: vec!["guarded_tool".to_string()],
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let approval = crate::approval::ApprovalManager::for_non_interactive(&risk_profile);
        let channel = ReplacingChannel;
        let ctx = TurnCtx {
            approval: Some(&approval),
            hooks: Some(&runner),
            channel: Some(&channel),
            ..test_ctx(&observer, &pacing, &tx)
        };

        let tool_calls = [parsed_call(
            "guarded_tool",
            serde_json::json!({"action": "run"}),
            "call-1",
        )];
        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();
        let prepared = prepare_tool_calls(
            &ctx,
            &[],
            None,
            &tool_calls,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        )
        .await
        .expect("preparation completes with the replaced call recorded");

        let (_, _, outcome) = prepared.ordered_results[0]
            .as_ref()
            .expect("replaced call records an outcome");
        assert!(
            outcome.success,
            "a replacement is a successful synthesized outcome"
        );
        assert_eq!(outcome.output, "safe replacement");
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "before:guarded_tool:test-turn:0:0".to_string(),
                "abandoned:guarded_tool:test-turn:0:0".to_string(),
            ],
            "a replacement that prevents execution abandons the context instead of leaving it pending"
        );
    }

    #[tokio::test]
    async fn repeated_prompt_preparation_failure_abandons_retained_batch() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, _rx) = mpsc::channel(16);
        let mut runner = crate::hooks::HookRunner::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        runner.register(Box::new(LifecycleRecorder {
            events: Arc::clone(&events),
            cancel_before_for: Vec::new(),
        }));
        let risk_profile = zeroclaw_config::schema::RiskProfileConfig {
            always_ask: vec!["shell".to_string()],
            auto_approve: vec!["normal_tool".to_string()],
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };
        let approval = crate::approval::ApprovalManager::for_non_interactive(&risk_profile);
        let ctx = lifecycle_ctx(&observer, &pacing, &tx, Some(&approval), Some(&runner));

        let mut seen = HashSet::new();
        let mut prompt_seen = HashSet::new();

        // First round: the normal call is retained; the shell call is denied by the
        // (non-interactive) gate and abandoned — but its prompt signature is
        // already recorded for the turn.
        let round0 = [
            parsed_call("normal_tool", serde_json::json!({"n": 1}), "call-1"),
            parsed_call("shell", serde_json::json!({"command": "ls"}), "call-2"),
        ];
        let prepared0 = prepare_tool_calls(
            &ctx,
            &[],
            None,
            &round0,
            &mut seen,
            &mut prompt_seen,
            0,
            false,
        )
        .await
        .expect("round 0 prepares");
        assert_eq!(prepared0.executable_indices, vec![0]);

        // Second round: a new normal call is retained, then the SAME prompt-required
        // shell call aborts the whole preparation batch.
        let round1 = [
            parsed_call("normal_tool", serde_json::json!({"n": 2}), "call-3"),
            parsed_call("shell", serde_json::json!({"command": "ls"}), "call-4"),
        ];
        let Err(error) = prepare_tool_calls(
            &ctx,
            &[],
            None,
            &round1,
            &mut seen,
            &mut prompt_seen,
            1,
            false,
        )
        .await
        else {
            panic!("the repeated prompt-required call must abort preparation");
        };
        assert!(error.to_string().contains("repeated prompt-required"));

        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "before:normal_tool:test-turn:0:0".to_string(),
                "before:shell:test-turn:0:1".to_string(),
                "abandoned:shell:test-turn:0:1".to_string(),
                "before:normal_tool:test-turn:1:0".to_string(),
                "before:shell:test-turn:1:1".to_string(),
                "abandoned:normal_tool:test-turn:1:0".to_string(),
                "abandoned:shell:test-turn:1:1".to_string(),
            ],
            "the aborted batch abandons every retained context exactly once, and round 0's denied call is not re-abandoned"
        );
    }

    #[tokio::test]
    async fn abandonment_batch_helper_skips_completed_positions() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, _rx) = mpsc::channel(8);
        let mut runner = crate::hooks::HookRunner::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        runner.register(Box::new(LifecycleRecorder {
            events: Arc::clone(&events),
            cancel_before_for: Vec::new(),
        }));
        let ctx = lifecycle_ctx(&observer, &pacing, &tx, None, Some(&runner));

        let executable_calls = vec![
            parsed_call("kept", serde_json::json!({"k": 1}), "call-1"),
            parsed_call("lost", serde_json::json!({"l": 1}), "call-3"),
        ];
        super::abandon_unexecuted_prepared_contexts(&ctx, 0, &[0, 2], &executable_calls, &[2])
            .await;

        assert_eq!(
            *events.lock().unwrap(),
            vec!["abandoned:kept:test-turn:0:0".to_string()],
            "positions whose after hook already ran are never abandoned"
        );
    }

    #[tokio::test]
    async fn abandonment_batch_helper_abandons_everything_when_nothing_completed() {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, _rx) = mpsc::channel(8);
        let mut runner = crate::hooks::HookRunner::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        runner.register(Box::new(LifecycleRecorder {
            events: Arc::clone(&events),
            cancel_before_for: Vec::new(),
        }));
        let ctx = lifecycle_ctx(&observer, &pacing, &tx, None, Some(&runner));

        let executable_calls = vec![
            parsed_call("first", serde_json::json!({"n": 1}), "call-1"),
            parsed_call("second", serde_json::json!({"n": 2}), "call-2"),
        ];
        super::abandon_unexecuted_prepared_contexts(&ctx, 3, &[0, 1], &executable_calls, &[]).await;

        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "abandoned:first:test-turn:3:0".to_string(),
                "abandoned:second:test-turn:3:1".to_string(),
            ],
            "a fully aborted execution batch abandons every executable context"
        );
    }
}
