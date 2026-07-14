//! Post-execution recording: result log line, the `after_tool_call` hook, the
//! completion Status, and filling the executed calls' `ordered_results` slots.

use super::call_prep::StreamToolCall;
use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
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
                    tool_role: stream_call.tool_role,
                    secs,
                    success: outcome.success,
                    error: outcome.error_reason.as_deref().map(scrub_credentials),
                })
                .await;
        }

        // Capture into the innermost live SOP step scope (no-op otherwise).
        if crate::sop::executor::step_capture_active() {
            crate::sop::executor::record_step_tool_call(
                &call.name,
                &call.arguments,
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
