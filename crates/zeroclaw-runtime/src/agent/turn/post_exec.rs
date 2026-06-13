//! Post-execution recording: result log line, the `after_tool_call` hook, the
//! completion Status, and filling the executed calls' `ordered_results` slots.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::util::truncate_with_ellipsis;
use zeroclaw_tool_call_parser::ParsedToolCall;

/// Record each executed tool call's outcome (upstream loop body,
/// post-execution section): one `tool_call_result` log line, the
/// `after_tool_call` hook, a completion Status to the draft, and the
/// call's slot in `ordered_results`.
pub(crate) async fn record_executed_outcomes(
    ctx: &TurnCtx<'_>,
    executable_indices: &[usize],
    executable_calls: &[ParsedToolCall],
    executed_outcomes: Vec<ToolExecutionOutcome>,
    ordered_results: &mut [Option<(String, Option<String>, ToolExecutionOutcome)>],
    iteration: usize,
) {
    for ((idx, call), outcome) in executable_indices
        .iter()
        .zip(executable_calls.iter())
        .zip(executed_outcomes)
    {
        if let Some(tx) = ctx.event_tx {
            super::events::emit_tool_call_pair(tx, call, &outcome).await;
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
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
                    "error_reason": outcome.error_reason,
                    "output": scrub_credentials(&outcome.output),
                    "trace_id": ctx.turn_id,
                })),
            "tool_call_result"
        );

        // ── Hook: after_tool_call (void) ─────────────────
        if let Some(hooks) = ctx.hooks {
            let tool_result_obj = crate::tools::ToolResult {
                success: outcome.success,
                output: outcome.output.clone(),
                error: None,
            };
            hooks
                .fire_after_tool_call(&call.name, &tool_result_obj, outcome.duration)
                .await;
        }

        // ── Progress: tool completion ───────────────────────
        if let Some(tx) = ctx.on_delta {
            let secs = outcome.duration.as_secs();
            let progress_msg = if outcome.success {
                format!("\u{2705} {} ({secs}s)\n", call.name)
            } else if let Some(ref reason) = outcome.error_reason {
                format!(
                    "\u{274c} {} ({secs}s): {}\n",
                    call.name,
                    truncate_with_ellipsis(reason, 200)
                )
            } else {
                format!("\u{274c} {} ({secs}s)\n", call.name)
            };
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"tool": call.name, "secs": secs})),
                "Sending progress complete to draft"
            );
            let _ = tx.send(StreamDelta::Status(progress_msg)).await;
        }

        ordered_results[*idx] = Some((call.name.clone(), call.tool_call_id.clone(), outcome));
    }
}
