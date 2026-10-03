//! Post-execution recording: result log line, the `after_tool_call` hook, the
//! completion Status, and filling the executed calls' `ordered_results` slots.

use super::call_prep::StreamToolCall;
use super::context::TurnCtx;
use super::events::{ProgressEvent, StreamDelta, send_progress};
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use zeroclaw_api::hook::ToolCallHookContext;
use zeroclaw_tool_call_parser::ParsedToolCall;

/// Record each executed tool call's outcome (upstream loop body,
/// post-execution section): one `tool_call_result` log line, the
/// `after_tool_call` hook, a completion Status to the draft, and the
/// call's slot in `ordered_results`.
pub(crate) async fn record_executed_outcomes(
    ctx: &TurnCtx<'_>,
    executable_indices: &[usize],
    executable_calls: &[ParsedToolCall],
    hook_contexts: &[Option<ToolCallHookContext>],
    stream_calls: &[Option<StreamToolCall>],
    executed_outcomes: Vec<ToolExecutionOutcome>,
    ordered_results: &mut [Option<(String, Option<String>, ToolExecutionOutcome)>],
    iteration: usize,
) {
    for ((((idx, call), hook_context), stream_call), outcome) in executable_indices
        .iter()
        .zip(executable_calls.iter())
        .zip(hook_contexts.iter())
        .zip(stream_calls.iter())
        .zip(executed_outcomes)
    {
        let sensitive_session_prompt =
            crate::agent::tool_execution::is_sensitive_session_prompt_tool(&call.name);
        // The pending ToolCall and terminal ToolResult are emitted by the
        // executor (execute_one_tool) at dispatch and completion time so serial
        // batches interleave call->result per tool. Post-exec only records the
        // outcome to history, logs, hooks, and ordered_results.

        if !sensitive_session_prompt {
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
        }

        // ── Hook: after_tool_call (void) ─────────────────
        if let (Some(hook_context), Some(hooks)) = (hook_context, ctx.hooks) {
            // A session-prompt call that began as such never has a hook
            // context. If a before hook rewrote an ordinary call into one, its
            // already-entered lifecycle must still terminate, but the attached
            // body and result remain outside hook visibility. A richer prompt
            // hook policy would be a separate architecture decision; this V1
            // projection intentionally carries only the final tool identity
            // and completion status.
            let (hook_args, tool_result_obj) = if sensitive_session_prompt {
                (
                    serde_json::json!({"session_prompt_payload": "omitted"}),
                    crate::tools::ToolResult {
                        success: outcome.success,
                        output: "[Session-prompt tool result omitted for hook privacy]".into(),
                        error: None,
                    },
                )
            } else {
                (
                    call.arguments.clone(),
                    crate::tools::ToolResult {
                        success: outcome.success,
                        output: outcome.output.clone().into(),
                        error: None,
                    },
                )
            };
            hooks
                .fire_after_tool_call_with_context_and_args(
                    hook_context,
                    &call.name,
                    &hook_args,
                    &tool_result_obj,
                    outcome.duration,
                )
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
