//! The max-iteration exit: when the loop exhausts its iterations, ask the
//! LLM for a tools-free final summary (with step timeout + cancel select)
//! and return it appended to the accumulated display text, or bail.

use super::outcome::ToolLoopCancelled;
use anyhow::Result;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::PacingConfig;
use zeroclaw_providers::{ChatMessage, ModelProvider};

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
    mut new_messages_out: Option<&mut Vec<ChatMessage>>,
    initial_history_len: usize,
) -> Result<String> {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "model": model,
                "max_iterations": max_iterations,
                "trace_id": turn_id,
            })),
        "tool_loop_exhausted"
    );

    // Graceful shutdown: ask the LLM for a final summary without tools
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({"max_iterations": max_iterations})),
        "Max iterations reached, requesting final summary"
    );
    history.push(ChatMessage::user(
        "You have reached the maximum number of tool iterations. \
         Please provide your best answer based on the work completed so far. \
         Summarize what you accomplished and what remains to be done."
            .to_string(),
    ));

    let summary_request = zeroclaw_providers::ChatRequest {
        messages: history,
        tools: None, // No tools — force a text response
        thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
            .try_with(Clone::clone)
            .ok()
            .flatten(),
    };
    let summary_future = model_provider.chat(summary_request, model, temperature);
    let summary_call = match pacing.step_timeout_secs {
        Some(step_secs) if step_secs > 0 => {
            let step_timeout = Duration::from_secs(step_secs);
            if let Some(token) = cancellation_token {
                tokio::select! {
                    () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                    result = tokio::time::timeout(step_timeout, summary_future) => match result {
                        Ok(inner) => inner,
                        Err(_) => anyhow::bail!(
                            "Final summary LLM call timed out after {step_secs}s (step_timeout_secs)"
                        ),
                    },
                }
            } else {
                match tokio::time::timeout(step_timeout, summary_future).await {
                    Ok(inner) => inner,
                    Err(_) => anyhow::bail!(
                        "Final summary LLM call timed out after {step_secs}s (step_timeout_secs)"
                    ),
                }
            }
        }
        _ => {
            if let Some(token) = cancellation_token {
                tokio::select! {
                    () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                    result = summary_future => result,
                }
            } else {
                summary_future.await
            }
        }
    };
    match summary_call {
        Ok(resp) => {
            let text = resp.text.unwrap_or_default();
            if text.is_empty() {
                anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
            }
            accumulated_display_text.push_str(&text);
            if let Some(out) = new_messages_out.as_deref_mut() {
                *out = history[initial_history_len..].to_vec();
            }
            Ok(accumulated_display_text)
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
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
            anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
        }
    }
}
