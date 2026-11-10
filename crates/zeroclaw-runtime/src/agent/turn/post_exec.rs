//! Post-execution recording: result log line, the `after_tool_call` hook, the
//! completion Status, and filling the executed calls' `ordered_results` slots.

use super::call_prep::StreamToolCall;
use super::context::TurnCtx;
use super::events::{ProgressEvent, StreamDelta, send_progress};
use super::redact::scrub_credentials;
use crate::agent::tool_execution::{CompletedToolExecution, ToolExecutionOutcome};
use zeroclaw_tool_call_parser::ParsedToolCall;

/// Move completed results into the round's canonical slots without awaiting
/// hooks or transport. The turn owner must append these slots before notifying.
pub(crate) fn retain_executed_outcomes(
    ctx: &TurnCtx<'_>,
    executable_indices: &[usize],
    executable_calls: &[ParsedToolCall],
    executed_outcomes: Vec<CompletedToolExecution>,
    ordered_results: &mut [Option<(String, Option<String>, ToolExecutionOutcome)>],
    iteration: usize,
) {
    for ((idx, call), completed) in executable_indices
        .iter()
        .zip(executable_calls)
        .zip(executed_outcomes)
    {
        let was_executed = completed.executed_tool_name.is_some();
        let outcome = completed.outcome;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(
                module_path!(),
                if was_executed {
                    ::zeroclaw_log::Action::Complete
                } else {
                    ::zeroclaw_log::Action::Note
                }
            )
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

        // The SOP ledger retains attempted calls too, including denied or
        // unknown calls with their failed outcomes. This is separate from
        // completion hooks, which require actual dispatch provenance.
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

/// Notify hooks and the draft only after the turn owner retains the whole round.
pub(crate) async fn notify_executed_outcomes(
    ctx: &TurnCtx<'_>,
    executable_indices: &[usize],
    executable_calls: &[ParsedToolCall],
    stream_calls: &[Option<StreamToolCall>],
    ordered_results: &[Option<(String, Option<String>, ToolExecutionOutcome)>],
    executed_calls: &[Option<ParsedToolCall>],
    iteration: usize,
) {
    for ((idx, call), stream_call) in executable_indices
        .iter()
        .zip(executable_calls.iter())
        .zip(stream_calls.iter())
    {
        let Some((_, _, outcome)) = &ordered_results[*idx] else {
            continue;
        };
        let was_executed = executed_calls[*idx].is_some();
        // The pending ToolCall and terminal ToolResult are emitted by the
        // executor (execute_one_tool) at dispatch and completion time so serial
        // batches interleave call->result per tool. Post-exec only records the
        // outcome to history, logs, hooks, and ordered_results.

        // ── Hook: after_tool_call (void) ─────────────────
        if let Some(hooks) = ctx.hooks {
            let hook_context = crate::hooks::tool_call_hook_context(ctx.turn_id, iteration, *idx);
            if !was_executed {
                hooks
                    .fire_tool_call_abandoned(&hook_context, &call.name)
                    .await;
            } else {
                let tool_result_obj = crate::tools::ToolResult {
                    success: outcome.success,
                    output: outcome.output.clone().into(),
                    error: None,
                };
                // The prepared arguments travel with the completion call so
                // argument-auditing hooks export what was actually dispatched and
                // never need to retain arguments between the hook phases.
                hooks
                    .fire_after_tool_call_with_context_and_args(
                        &hook_context,
                        &call.name,
                        &call.arguments,
                        &tool_result_obj,
                        outcome.duration,
                    )
                    .await;
            }
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
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_executed_outcomes(
    ctx: &TurnCtx<'_>,
    executable_indices: &[usize],
    executable_calls: &[ParsedToolCall],
    stream_calls: &[Option<StreamToolCall>],
    outcomes: Vec<CompletedToolExecution>,
    ordered_results: &mut [Option<(String, Option<String>, ToolExecutionOutcome)>],
    iteration: usize,
) {
    let mut dispatched = vec![None; ordered_results.len()];
    for ((index, call), outcome) in executable_indices
        .iter()
        .zip(executable_calls)
        .zip(&outcomes)
    {
        if outcome.executed_tool_name.is_some() {
            dispatched[*index] = Some(call.clone());
        }
    }
    retain_executed_outcomes(
        ctx,
        executable_indices,
        executable_calls,
        outcomes,
        ordered_results,
        iteration,
    );
    notify_executed_outcomes(
        ctx,
        executable_indices,
        executable_calls,
        stream_calls,
        ordered_results,
        &dispatched,
        iteration,
    )
    .await;
}
