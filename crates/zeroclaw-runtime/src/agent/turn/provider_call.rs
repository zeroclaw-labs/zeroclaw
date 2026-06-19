//! The provider call step: request announcement, budget enforcement, and the
//! streaming/non-streaming chat dispatch.

use super::context::TurnCtx;
use super::events::StreamDelta;
use super::outcome::{StreamInterruptedAfterOutput, ToolLoopCancelled, is_tool_loop_cancelled};
use super::redact::scrub_credentials;
use super::stream_consume::consume_provider_streaming_response;
use crate::agent::cost::check_tool_loop_budget;
use crate::cost::types::BudgetCheck;
use crate::observability::ObserverEvent;
use crate::tools::ToolSpec;
use anyhow::Result;
use std::time::{Duration, Instant};
use zeroclaw_providers::{ChatMessage, ChatRequest, ChatResponse, ModelProvider, ProviderDispatch};

/// Result of one provider call.
///
/// CANCEL ASYMMETRY — preserved verbatim from the pre-extraction loop body
/// (RUN_SHEET `turn.provider_call`, plan flag §8.7):
/// - The non-streaming cancel paths (and the step-timeout bails) return the
///   OUTER `Err` from [`call_provider`] — the loop propagates it directly,
///   skipping observer-failure recording and context-overflow recovery.
/// - The streaming-fallback cancel yields `Err` as the `chat_result` VALUE —
///   it flows through the loop's `match chat_result` Err arm (observer
///   failure + recovery) exactly as before.
/// - A cancel that fires while consuming the stream is also an inner `Err`
///   (and skips the non-streaming fallback entirely): the loop records the
///   observer failure with the fixed cancellation message, matching the
///   pre-consolidation streaming engine.
pub(crate) struct ProviderCallOutcome {
    pub(crate) chat_result: Result<ChatResponse>,
    pub(crate) streamed_live_deltas: bool,
    pub(crate) streamed_protocol_suppressed: bool,
    pub(crate) streamed_visible_text: String,
}

/// Announce the upcoming LLM request: progress Status, observer `LlmRequest`,
/// `llm_request` log line, and the `fire_llm_input` hook.
///
/// Returns `llm_started_at`, taken between the log line and the hook so the
/// measured LLM duration includes the hook await — identical to the
/// pre-extraction ordering.
pub(crate) async fn announce_llm_request(
    ctx: &TurnCtx<'_>,
    history: &[ChatMessage],
    active_model_provider: &dyn ModelProvider,
    active_model_provider_name: &str,
    active_model: &str,
    iteration: usize,
) -> Instant {
    // ── Progress: LLM thinking ────────────────────────────
    if let Some(tx) = ctx.on_delta {
        let phase = if iteration == 0 {
            "\u{1f914} Thinking...\n".to_string()
        } else {
            format!("\u{1f914} Thinking (round {})...\n", iteration + 1)
        };
        let _ = tx.send(StreamDelta::Status(phase)).await;
    }

    ctx.observer.record_event(&ObserverEvent::LlmRequest {
        model_provider: active_model_provider_name.to_string(),
        model: active_model.to_string(),
        messages_count: history.len(),
        channel: None,
        agent_alias: None,
        turn_id: None,
    });
    {
        let _provider_guard = ::zeroclaw_log::attribution_span!(active_model_provider).entered();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send).with_attrs(
                ::serde_json::json!({
                    "iteration": iteration + 1,
                    "messages_count": history.len(),
                    "model": active_model,
                    "trace_id": ctx.turn_id,
                })
            ),
            "llm_request"
        );
    }

    let llm_started_at = Instant::now();

    // Fire void hook before LLM call
    if let Some(hooks) = ctx.hooks {
        hooks.fire_llm_input(history, ctx.model).await;
    }

    llm_started_at
}

/// Budget enforcement — block if limit exceeded (no-op when not scoped).
pub(crate) fn enforce_tool_loop_budget() -> Result<()> {
    if let Some(BudgetCheck::Exceeded {
        current_usd,
        limit_usd,
        period,
    }) = check_tool_loop_budget()
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "current_usd": current_usd,
                    "limit_usd": limit_usd,
                    "period": format!("{period:?}"),
                })),
            "tool-call loop budget exceeded"
        );
        anyhow::bail!(
            "Budget exceeded: ${:.4} of ${:.2} {:?} limit. Cannot make further API calls until the budget resets.",
            current_usd,
            limit_usd,
            period
        );
    }
    Ok(())
}

/// One provider call: streaming via `consume_provider_streaming_response`
/// with non-streaming fallback, or plain non-streaming chat with optional
/// per-step timeout and cancel select. See [`ProviderCallOutcome`] for the
/// cancel asymmetry this function must preserve.
pub(crate) async fn call_provider(
    ctx: &TurnCtx<'_>,
    active_model_provider: &dyn ModelProvider,
    active_model: &str,
    prepared_messages: &[ChatMessage],
    request_tools: Option<&[ToolSpec]>,
    should_consume_provider_stream: bool,
    iteration: usize,
) -> Result<ProviderCallOutcome> {
    let mut streamed_live_deltas = false;
    let mut streamed_protocol_suppressed = false;
    let mut streamed_visible_text = String::new();

    let chat_result = if should_consume_provider_stream {
        // Attribution is opened by ProviderDispatch::from_ref(...).stream_chat
        // inside `consume_provider_streaming_response`; the caller does not
        // wrap a second attribution_span! here.
        let stream_future = consume_provider_streaming_response(
            active_model_provider,
            prepared_messages,
            request_tools,
            active_model,
            ctx.temperature,
            ctx.cancellation_token,
            ctx.on_delta,
            ctx.event_tx,
            ctx.strict_tool_parsing,
        );
        match stream_future.await {
            Ok(streamed) => {
                streamed_live_deltas = streamed.forwarded_live_deltas;
                streamed_protocol_suppressed = streamed.suppressed_protocol;
                streamed_visible_text = streamed.forwarded_visible_text;
                let reasoning_content = if streamed.reasoning_content.is_empty() {
                    None
                } else {
                    Some(streamed.reasoning_content)
                };
                Ok(zeroclaw_providers::ChatResponse {
                    text: Some(streamed.response_text),
                    tool_calls: streamed.tool_calls,
                    usage: streamed.usage,
                    reasoning_content,
                })
            }
            Err(stream_err)
                if is_tool_loop_cancelled(&stream_err)
                    || stream_err
                        .downcast_ref::<StreamInterruptedAfterOutput>()
                        .is_some() =>
            {
                // No fallback: the consumer either cancelled the turn (a
                // retry is a doomed request) or already saw streamed output
                // (a retry duplicates visible text on append-only
                // consumers). Surfaced as the inner chat_result so the
                // loop's Err arm records the observer failure, exactly as
                // the pre-consolidation streaming engine did.
                Err(stream_err)
            }
            Err(stream_err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "model": active_model,
                            "iteration": iteration + 1,
                            "error": scrub_credentials(&stream_err.to_string()),
                            "trace_id": ctx.turn_id,
                        })),
                    "llm_stream_fallback: provider stream failed, falling back to non-streaming chat"
                );
                {
                    let dispatcher = ProviderDispatch::from_ref(active_model_provider);
                    let chat_future = dispatcher.chat(
                        ChatRequest {
                            messages: prepared_messages,
                            tools: request_tools,
                            thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
                                .try_with(Clone::clone)
                                .ok()
                                .flatten(),
                        },
                        active_model,
                        ctx.temperature,
                    );
                    if let Some(token) = ctx.cancellation_token {
                        tokio::select! {
                            () = token.cancelled() => Err(ToolLoopCancelled.into()),
                            result = chat_future => result,
                        }
                    } else {
                        chat_future.await
                    }
                }
            }
        }
    } else {
        // Non-streaming path: wrap with optional per-step timeout from
        // pacing config to catch hung model responses.
        let dispatcher = ProviderDispatch::from_ref(active_model_provider);
        let chat_future = dispatcher.chat(
            ChatRequest {
                messages: prepared_messages,
                tools: request_tools,
                thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
                    .try_with(Clone::clone)
                    .ok()
                    .flatten(),
            },
            active_model,
            ctx.temperature,
        );

        match ctx.pacing.step_timeout_secs {
            Some(step_secs) if step_secs > 0 => {
                let step_timeout = Duration::from_secs(step_secs);
                if let Some(token) = ctx.cancellation_token {
                    tokio::select! {
                        () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                        result = tokio::time::timeout(step_timeout, chat_future) => {
                            match result {
                                Ok(inner) => inner,
                                Err(_) => anyhow::bail!(
                                    "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                                ),
                            }
                        },
                    }
                } else {
                    match tokio::time::timeout(step_timeout, chat_future).await {
                        Ok(inner) => inner,
                        Err(_) => anyhow::bail!(
                            "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                        ),
                    }
                }
            }
            _ => {
                if let Some(token) = ctx.cancellation_token {
                    tokio::select! {
                        () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                        result = chat_future => result,
                    }
                } else {
                    chat_future.await
                }
            }
        }
    };

    Ok(ProviderCallOutcome {
        chat_result,
        streamed_live_deltas,
        streamed_protocol_suppressed,
        streamed_visible_text,
    })
}
