//! Post-execution recording: result log line, the `after_tool_call` hook, the
//! completion Status, and filling the executed calls' `ordered_results` slots.

use super::call_prep::StreamToolCall;
use super::context::TurnCtx;
use super::events::{ProgressEvent, StreamDelta, send_progress};
use super::redact::{loggable_args_value, scrub_credentials};
use crate::agent::tool_execution::ToolExecutionOutcome;
use zeroclaw_tool_call_parser::ParsedToolCall;

/// Record each executed tool call's outcome (upstream loop body,
/// post-execution section): one `tool_call_result` log line, the
/// `after_tool_call` hook, a completion Status to the draft, and the
/// call's slot in `ordered_results`.
pub(crate) async fn record_executed_outcomes(
    ctx: &TurnCtx<'_>,
    executable_indices: &[usize],
    executable_calls: &[ParsedToolCall],
    stream_calls: &[Option<StreamToolCall>],
    executed_outcomes: Vec<ToolExecutionOutcome>,
    ordered_results: &mut [Option<(String, Option<String>, ToolExecutionOutcome)>],
    iteration: usize,
) {
    for (((idx, call), stream_call), outcome) in executable_indices
        .iter()
        .zip(executable_calls.iter())
        .zip(stream_calls.iter())
        .zip(executed_outcomes)
    {
        // The pending ToolCall and terminal ToolResult are emitted by the
        // executor (execute_one_tool) at dispatch and completion time so serial
        // batches interleave call->result per tool. Post-exec only records the
        // outcome to history, logs, hooks, and ordered_results.

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                .with_category(::zeroclaw_log::EventCategory::Tool)
                .with_outcome(if outcome.success {
                    ::zeroclaw_log::EventOutcome::Success
                } else {
                    ::zeroclaw_log::EventOutcome::Failure
                })
                .with_duration(u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX))
                .with_attrs(::serde_json::json!({
                    "model": ctx.model,
                    "iteration": iteration + 1,
                    "tool": call.name.clone(),
                    "error_reason": outcome.error_reason.as_deref().map(scrub_credentials),
                    "output": scrub_credentials(&outcome.output),
                    "trace_id": ctx.turn_id,
                })),
            "tool_call_result"
        );

        // ── Hook: after_tool_call (void) ─────────────────
        if let Some(hooks) = ctx.hooks {
            let tool_result_obj = crate::tools::ToolResult {
                success: outcome.success,
                output: outcome.output.clone().into(),
                error: None,
            };
            hooks
                .fire_after_tool_call(&call.name, &tool_result_obj, outcome.duration)
                .await;
        }

        // ── Progress: tool completion ───────────────────────
        send_progress(ctx.on_delta, ProgressEvent::Planning).await;
        if let (Some(tx), Some(stream_call)) = (ctx.on_delta, stream_call) {
            let secs = outcome.duration.as_secs();
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Tool)
                    .with_attrs(::serde_json::json!({"tool": call.name, "secs": secs})),
                "Sending progress complete to draft"
            );
            let _ = tx
                .send(StreamDelta::ToolComplete {
                    tool: call.name.clone(),
                    arguments: std::sync::Arc::clone(&stream_call.arguments),
                    tool_provenance: stream_call.tool_provenance,
                    secs,
                    success: outcome.success,
                    error: outcome.error_reason.as_deref().map(scrub_credentials),
                })
                .await;
        }

        // Capture into the innermost live SOP step scope (no-op otherwise).
        if crate::sop::executor::step_capture_active() {
            let capture_args = loggable_args_value(ctx.tool_by_name(&call.name), &call.arguments);
            crate::sop::executor::record_step_tool_call(
                &call.name,
                &capture_args,
                outcome.success,
                outcome.output.clone(),
                outcome.output_data.clone(),
                outcome.error_reason.as_deref(),
                u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX),
            );
        }

        ordered_results[*idx] = Some((call.name.clone(), call.tool_call_id.clone(), outcome));
    }
}
#[cfg(test)]
mod tests {
    use super::record_executed_outcomes;
    use crate::agent::tool_execution::ToolExecutionOutcome;
    use crate::observability::NoopObserver;
    use crate::tools::config_patch::ConfigPatchTool;
    use std::sync::Arc;
    use std::time::Duration;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::{PacingConfig, StreamReasoningMode};
    use zeroclaw_tool_call_parser::ParsedToolCall;

    #[tokio::test]
    async fn sop_capture_uses_the_tools_secret_aware_argument_projection() {
        let sentinel = "sentinel-sop-secret-must-not-leak";
        let dir = tempfile::tempdir().unwrap();
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ConfigPatchTool::new(
            dir.path().join("config.toml"),
            Arc::new(SecurityPolicy::default()),
        ))];
        let pacing = PacingConfig::default();
        let observer = NoopObserver;
        let ctx = super::TurnCtx {
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: None,
            approval: None,
            channel_name: "test",
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
            turn_id: "sop-redaction-test",
            agent_alias: None,
            parent_agent_alias: None,
            tools: &tools,
        };
        let call = ParsedToolCall {
            name: "config_patch".to_string(),
            arguments: serde_json::json!({
                "ops": [{
                    "op": "add",
                    "path": "/providers/models/openai/default/api_key",
                    "value": sentinel
                }]
            }),
            tool_call_id: Some("call-1".to_string()),
        };
        let outcome = ToolExecutionOutcome {
            output: "ok".to_string(),
            output_data: None,
            success: true,
            error_reason: None,
            duration: Duration::ZERO,
            receipt: None,
        };
        let mut ordered_results = vec![None];
        let sink = crate::sop::executor::new_step_call_sink();

        crate::sop::executor::scope_step_call_sink(sink.clone(), async {
            record_executed_outcomes(
                &ctx,
                &[0],
                &[call],
                &[None],
                vec![outcome],
                &mut ordered_results,
                0,
            )
            .await;
        })
        .await;

        let captured = crate::sop::executor::drain_step_calls(&sink);
        assert_eq!(captured.len(), 1);
        let rendered = captured[0].args.to_string();
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(!rendered.contains(sentinel), "{rendered}");
    }
}
