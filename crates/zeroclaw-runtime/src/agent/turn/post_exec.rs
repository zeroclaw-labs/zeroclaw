//! Post-execution recording: result log line, the `after_tool_call` hook, the
//! completion Status, and filling the executed calls' `ordered_results` slots.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::progress::render_tool_completion_progress;
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
    executed_outcomes: Vec<ToolExecutionOutcome>,
    ordered_results: &mut [Option<(String, Option<String>, ToolExecutionOutcome)>],
    iteration: usize,
) {
    for ((idx, call), outcome) in executable_indices
        .iter()
        .zip(executable_calls.iter())
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
            let progress_msg = render_tool_completion_progress(
                &call.name,
                &call.arguments,
                secs,
                outcome.success,
                outcome.error_reason.as_deref(),
                ctx.stream_tool_arguments,
            );
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Tool)
                    .with_attrs(::serde_json::json!({"tool": call.name, "secs": secs})),
                "Sending progress complete to draft"
            );
            let _ = tx.send(StreamDelta::Status(progress_msg)).await;
        }

        ordered_results[*idx] = Some((call.name.clone(), call.tool_call_id.clone(), outcome));
    }
}

#[cfg(test)]
mod tests {
    use super::render_tool_completion_progress;
    use serde_json::json;

    /// The CLI progress line is a rendering surface, so credential-shaped
    /// failure text must be scrubbed even though `error_reason` is raw on the
    /// data path.
    #[test]
    fn completion_progress_scrubs_credential_error_reason() {
        let line = render_tool_completion_progress(
            "config_read",
            &json!({}),
            2,
            false,
            Some("api_key = \"sk-live-abcd1234efgh5678\""),
            None,
        );
        assert!(
            line.contains("[REDACTED]"),
            "expected scrubbed line: {line}"
        );
        assert!(
            !line.contains("abcd1234efgh5678"),
            "raw secret leaked: {line}"
        );
    }

    #[test]
    fn completion_progress_success_has_no_error_text() {
        let line = render_tool_completion_progress("echo", &json!({}), 0, true, None, None);
        assert!(line.starts_with('\u{2705}'));
        assert!(!line.contains(':'));
    }
}
