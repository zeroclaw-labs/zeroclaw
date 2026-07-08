//! The max-iteration exit: when the loop exhausts its iterations, ask the
//! LLM for a tools-free final summary (with step timeout + cancel select)
//! and return it appended to the accumulated display text, or bail.

use super::knobs::{LoopKnobs, MaxIterationBehavior};
use super::outcome::ToolLoopCancelled;
use anyhow::Result;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::PacingConfig;
use zeroclaw_providers::{ChatMessage, ModelProvider, ProviderDispatch};

/// Graceful shutdown after the loop exhausts `max_iterations` (upstream loop
/// body, max-iteration exit): log exhaustion, push a summary-request user
/// message, make a tools-free `chat` call honoring `pacing.step_timeout_secs`
/// and the cancellation token, and return `Ok(accumulated + summary)` — or
/// bail with "exceeded maximum tool iterations" when the summary is empty or
/// the call fails.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn finish_after_max_iterations(
    model_provider: &dyn ModelProvider,
    history: &mut Vec<ChatMessage>,
    provider_name: &str,
    model: &str,
    temperature: Option<f64>,
    pacing: &PacingConfig,
    cancellation_token: Option<&CancellationToken>,
    max_iterations: usize,
    mut accumulated_display_text: String,
    turn_id: &str,
    knobs: &LoopKnobs,
    mut new_messages_out: Option<&mut Vec<ChatMessage>>,
) -> Result<String> {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "model": model,
                "max_iterations": max_iterations,
                "trace_id": turn_id,
            })),
        "tool_loop_exhausted"
    );

    // ErrorAtCap callers (embedders driving Agent::turn) treat the cap as a
    // control signal: bail instead of spending another LLM call on a summary.
    if knobs.max_iteration_behavior == MaxIterationBehavior::ErrorAtCap {
        anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
    }

    // Graceful shutdown: ask the LLM for a final summary without tools
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({"max_iterations": max_iterations})),
        "Max iterations reached, requesting final summary"
    );
    // Sanitise tool_use / tool_result pairing before the graceful-shutdown
    // request. When the loop exits immediately after the model emits a
    // tool_use (hitting max_tool_iterations before the runner records a
    // tool_result), the history carries an unpaired tool_use block.
    // Bedrock/Anthropic reject the follow-up tools-free summary call with:
    // "Expected toolResult blocks at messages.N.content for the following
    // Ids: tooluse_*". Two complementary sweeps:
    //   1. strip_orphaned_tool_calls_from_assistants — removes tool_calls from
    //      assistant messages whose ids have no following tool result.
    //   2. remove_orphaned_tool_messages — removes tool-role messages that no
    //      longer have a matching assistant (symmetric case).
    let tool_calls_stripped =
        crate::agent::history_pruner::strip_orphaned_tool_calls_from_assistants(history);
    let tool_messages_removed =
        crate::agent::history_pruner::remove_orphaned_tool_messages(history).removed;
    if tool_calls_stripped > 0 || tool_messages_removed > 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "tool_calls_stripped": tool_calls_stripped,
                    "tool_messages_removed": tool_messages_removed,
                })),
            "Sanitised orphaned tool_use/tool_result pairing before graceful shutdown"
        );
    }

    let summary_prompt = ChatMessage::user(
        "You have reached the maximum number of tool iterations. \
         Please provide your best answer based on the work completed so far. \
         Summarize what you accomplished and what remains to be done."
            .to_string(),
    );
    // Pushed into history for the request below, but mirrored into the
    // append-log (and kept in history) only when the summary call SUCCEEDS:
    // a failed/cancelled/timed-out/empty summary must not persist an
    // unanswered synthetic prompt into wrapper transcripts — every failure
    // exit pops it back off.
    let summary_prompt_mirror = summary_prompt.clone();
    history.push(summary_prompt);

    enum SummaryCall {
        Cancelled,
        TimedOut(u64),
        Done(Result<zeroclaw_providers::ChatResponse>),
    }
    let summary_call = {
        let summary_request = zeroclaw_providers::ChatRequest {
            messages: history,
            tools: None, // No tools — force a text response
            thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
                .try_with(Clone::clone)
                .ok()
                .flatten(),
        };
        let dispatcher = ProviderDispatch::from_ref(model_provider);
        let summary_future = dispatcher.chat(summary_request, model, temperature);
        match pacing.step_timeout_secs {
            Some(step_secs) if step_secs > 0 => {
                let step_timeout = Duration::from_secs(step_secs);
                if let Some(token) = cancellation_token {
                    tokio::select! {
                        () = token.cancelled() => SummaryCall::Cancelled,
                        result = tokio::time::timeout(step_timeout, summary_future) => match result {
                            Ok(inner) => SummaryCall::Done(inner),
                            Err(_) => SummaryCall::TimedOut(step_secs),
                        },
                    }
                } else {
                    match tokio::time::timeout(step_timeout, summary_future).await {
                        Ok(inner) => SummaryCall::Done(inner),
                        Err(_) => SummaryCall::TimedOut(step_secs),
                    }
                }
            }
            _ => {
                if let Some(token) = cancellation_token {
                    tokio::select! {
                        () = token.cancelled() => SummaryCall::Cancelled,
                        result = summary_future => SummaryCall::Done(result),
                    }
                } else {
                    SummaryCall::Done(summary_future.await)
                }
            }
        }
    };

    let resp = match summary_call {
        SummaryCall::Cancelled => {
            history.pop();
            return Err(ToolLoopCancelled.into());
        }
        SummaryCall::TimedOut(step_secs) => {
            history.pop();
            anyhow::bail!("Final summary LLM call timed out after {step_secs}s (step_timeout_secs)")
        }
        SummaryCall::Done(Err(e)) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Provider)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "model": model,
                        "provider": provider_name,
                        "max_iterations": max_iterations,
                        "trace_id": turn_id,
                        "error": format!("{e}"),
                    })),
                "final summary LLM call failed after iteration exhaustion; bailing"
            );
            history.pop();
            anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
        }
        SummaryCall::Done(Ok(resp)) => resp,
    };

    let text = resp.text.unwrap_or_default();
    if text.is_empty() {
        history.pop();
        anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
    }
    // Persist the answered prompt + summary like every other final assistant
    // response: without the summary message, persistent-history callers (the
    // streamed wrapper's replay, new_messages consumers) store a transcript
    // ending on the synthetic user prompt with no answer — the delivered
    // summary would be absent and the model re-answers the synthetic prompt
    // next turn.
    let summary_msg = ChatMessage::assistant(text.clone());
    if let Some(out) = &mut new_messages_out {
        out.push(summary_prompt_mirror);
        out.push(summary_msg.clone());
    }
    history.push(summary_msg);
    accumulated_display_text.push_str(&text);
    Ok(accumulated_display_text)
}
