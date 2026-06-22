//! LLM-failure recording and in-loop context-overflow recovery.

use super::outcome::is_tool_loop_cancelled;
use crate::agent::history::estimate_history_tokens;
use crate::agent::history_trim::trim_to_recent_turns;
use crate::observability::{Observer, ObserverEvent};
use std::time::Instant;
use zeroclaw_providers::ChatMessage;

/// Record a failed provider call: observer `LlmResponse` (failure) and the
/// `llm_response` failure log line.
pub(crate) fn record_llm_failure(
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    llm_started_at: Instant,
    iteration: usize,
    turn_id: &str,
    e: &anyhow::Error,
) {
    // User cancellation gets the fixed message the streaming consumers have
    // always seen (and pin), never a raw error string.
    let safe_error = if is_tool_loop_cancelled(e) {
        "request cancelled by user".to_string()
    } else {
        zeroclaw_providers::sanitize_api_error(&e.to_string())
    };
    observer.record_event(&ObserverEvent::LlmResponse {
        model_provider: provider_name.to_string(),
        model: model.to_string(),
        duration: llm_started_at.elapsed(),
        success: false,
        error_message: Some(safe_error.clone()),
        input_tokens: None,
        output_tokens: None,
        channel: None,
        agent_alias: None,
        turn_id: None,
    });
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_category(::zeroclaw_log::EventCategory::Provider)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_duration(u64::try_from(llm_started_at.elapsed().as_millis()).unwrap_or(u64::MAX))
            .with_attrs(::serde_json::json!({
                "model": model,
                "iteration": iteration + 1,
                "error": safe_error,
                "trace_id": turn_id,
            })),
        "llm_response"
    );
}

/// Context overflow recovery: trim history and retry.
///
/// Returns `true` when the history was trimmed and the caller should
/// `continue` the loop; the orchestrator keeps
/// `if recovered { continue; } return Err(e);` inline.
pub(crate) fn try_recover_context_overflow(
    history: &mut Vec<ChatMessage>,
    e: &anyhow::Error,
    iteration: usize,
) -> bool {
    if zeroclaw_providers::reliable::is_context_window_exceeded(e) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_attrs(::serde_json::json!({"iteration": iteration + 1})),
            "Context window exceeded, attempting in-loop recovery"
        );

        // One rule: drop oldest whole turns until we are under a budget
        // forced below the current size. Never splits a tool_use/tool_result
        // pair, never silently shrinks a result. Whole turns or nothing.
        let tokens_now = estimate_history_tokens(history);
        let budget = tokens_now.saturating_mul(2) / 3;
        let owned = std::mem::take(history);
        let result = trim_to_recent_turns(owned, budget);
        let trimmed = result.trimmed;
        let dropped_turns = result.dropped_turns;
        let dropped_messages = result.dropped_messages;
        let tokens_after = result.tokens_after;
        *history = result.history;
        if trimmed {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Retry)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({
                        "dropped_turns": dropped_turns,
                        "dropped_messages": dropped_messages,
                        "tokens_before": tokens_now,
                        "tokens_after": tokens_after,
                    })),
                "Context recovery: dropped oldest whole turns, retrying"
            );
            return true;
        }

        // Nothing left to trim — truly unrecoverable
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "Context overflow unrecoverable: only one turn left, cannot trim further"
        );
    }
    false
}
