//! The per-call preparation loop: `before_tool_call` hook, delivery defaults,
//! the approval gate, the duplicate-call gate, and start logging — producing
//! the executable subset of this round's tool calls.

use super::approval_gate::{ApprovalGateOutcome, gate_tool_approval};
use super::context::TurnCtx;
use super::delivery_defaults::maybe_inject_channel_delivery_defaults;
use super::events::{ProgressEvent, StreamDelta, emit_tool_call_pair, send_progress};
use super::redact::{loggable_args_string, scrub_credentials, streamable_args_value};
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::attribution::ToolProvenance;
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
        arguments: streamable_args_value(ctx.tool_by_name(tool_name), tool_args),
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
                anyhow::bail!("{repeated}");
            }
        }

        // ── Approval hook ────────────────────────────────
        let approved = match gate_tool_approval(ctx, &tool_name, &mut tool_args, iteration).await {
            ApprovalGateOutcome::Proceed { approved } => approved,
            ApprovalGateOutcome::Deny(outcome) | ApprovalGateOutcome::Replace(outcome) => {
                // Streaming consumers see the denied/replaced call and its
                // synthesized result (e.g. a DenyWithEdit replacement) as a
                // ToolCall/ToolResult pair, as the direct path always did.
                emit_synthetic_tool_call_pair(ctx, call, &tool_name, &tool_args, &outcome).await;
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
                    "arguments": loggable_args_string(ctx.tool_by_name(&tool_name), &tool_args),
                    "trace_id": ctx.turn_id,
                })),
            "tool_call_start"
        );

        // ── Progress: tool start ────────────────────────────
        send_progress(ctx.on_delta, ProgressEvent::RunningTool).await;
        let stream_call = ctx.on_delta.map(|_| StreamToolCall {
            arguments: Arc::new(streamable_args_value(
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
            Ok(Some(ChannelApprovalResponse::Approve))
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
            name: String,
            _args: serde_json::Value,
        ) -> HookResult<(String, serde_json::Value)> {
            HookResult::Continue((name, self.arguments.clone()))
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

    fn test_ctx<'a>(
        observer: &'a NoopObserver,
        pacing: &'a PacingConfig,
        on_delta: &'a mpsc::Sender<DraftEvent>,
        tools: &'a [Box<dyn Tool>],
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
            tools,
        }
    }

    async fn emitted_tool_provenance(
        tools_registry: Vec<Box<dyn Tool>>,
        tool_name: &str,
    ) -> Vec<Option<ToolProvenance>> {
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (tx, mut rx) = mpsc::channel(4);
        let ctx = test_ctx(&observer, &pacing, &tx, &tools_registry);
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
        let ctx = test_ctx(&observer, &pacing, &tx, &tools);
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

    #[tokio::test]
    async fn denied_config_patch_emits_only_the_redacted_post_hook_call() {
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
            config_path,
            Arc::new(SecurityPolicy::default()),
        ))];
        let sentinel = "sentinel-denied-event-secret-01234567";
        let rewritten_path = "/http_request/secrets/api_token";
        let mut hooks = HookRunner::new();
        hooks.register(Box::new(RewriteConfigPatchHook {
            arguments: config_patch_args(rewritten_path, sentinel),
        }));
        let profile = RiskProfileConfig {
            level: zeroclaw_config::autonomy::AutonomyLevel::Supervised,
            always_ask: vec!["config_patch".to_string()],
            ..RiskProfileConfig::default()
        };
        let approval = ApprovalManager::for_non_interactive(&profile);
        let channel = ApprovingChatChannel {
            asked: AtomicUsize::new(0),
        };
        let observer = NoopObserver;
        let pacing = PacingConfig::default();
        let (event_tx, mut event_rx) = mpsc::channel(4);
        let ctx = TurnCtx {
            observer: &observer,
            provider_name: "test",
            model: "test-model",
            temperature: None,
            approval: Some(&approval),
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
        };
        let calls = [ParsedToolCall {
            name: "config_patch".to_string(),
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
            0,
            "ordinary chat must never receive an operator-only prompt"
        );
        assert_synthetic_pair_is_redacted(&mut event_rx, sentinel, rewritten_path).await;
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
        };
        let calls = [ParsedToolCall {
            name: "config_patch".to_string(),
            arguments: config_patch_args(secret_path, sentinel),
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
}
