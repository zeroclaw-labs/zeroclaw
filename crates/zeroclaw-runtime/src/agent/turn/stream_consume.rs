//! Streaming provider-response consumption for the turn loop.

use super::events::{DraftEvent, StreamDelta};
use super::outcome::{StreamCancelledAfterOutput, StreamInterruptedAfterOutput, ToolLoopCancelled};
use super::stream_guard::{StreamTextGuard, StreamThinkTagStripper};
use anyhow::Result;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::model_provider::StreamEvent;
use zeroclaw_providers::{ChatMessage, ChatRequest, ModelProvider, ToolCall};

#[derive(Debug, Default)]
pub(crate) struct StreamedChatOutcome {
    pub(crate) response_text: String,
    /// Accumulated reasoning/thinking content from streaming deltas.
    ///
    /// Captured separately from `response_text` so it can be threaded into
    /// `ChatResponse.reasoning_content` and ultimately persisted on the
    /// `AssistantToolCalls` history entry. Required for model_providers like
    /// DeepSeek V4 that reject follow-up requests when the assistant's
    /// prior `reasoning_content` is missing from replayed tool-call turns
    ///.
    pub(crate) reasoning_content: String,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) forwarded_live_deltas: bool,
    pub(crate) suppressed_protocol: bool,
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
}

pub(crate) async fn consume_provider_streaming_response(
    model_provider: &dyn ModelProvider,
    messages: &[ChatMessage],
    request_tools: Option<&[crate::tools::ToolSpec]>,
    model: &str,
    temperature: Option<f64>,
    cancellation_token: Option<&CancellationToken>,
    on_delta: Option<&tokio::sync::mpsc::Sender<DraftEvent>>,
    event_tx: Option<&tokio::sync::mpsc::Sender<TurnEvent>>,
    strict_tool_parsing: bool,
) -> Result<StreamedChatOutcome> {
    let mut provider_stream = model_provider.stream_chat(
        ChatRequest {
            messages,
            tools: request_tools,
            thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
                .try_with(Clone::clone)
                .ok()
                .flatten(),
        },
        model,
        temperature,
        zeroclaw_providers::traits::StreamOptions::new(true),
    );
    let mut outcome = StreamedChatOutcome::default();
    let mut delta_sender = on_delta;
    let mut suppress_forwarding = false;
    let mut text_guard = StreamTextGuard::new(request_tools);
    let mut think_stripper = StreamThinkTagStripper::default();
    // Correlates PreExecutedToolCall events with their later results so both
    // TurnEvents share a stable id (FIFO per tool name).
    let mut pre_executed_ids: std::collections::HashMap<
        String,
        std::collections::VecDeque<String>,
    > = std::collections::HashMap::new();
    // Tracks event_tx-visible output only (Chunk/Thinking/pre-executed tool
    // events). Draft (`on_delta`) forwards don't count: drafts are mutable
    // surfaces, so a non-streaming retry after a stream error overwrites
    // rather than duplicates.
    let mut visible_event_output = false;
    // Exactly the text forwarded as `TurnEvent::Chunk` — what an event_tx
    // consumer actually SAW. On interruption this (never the raw
    // accumulated `response_text`, which includes guard-withheld and
    // suppression-buffered text) is the partial that may be persisted as
    // already-delivered output.
    let mut forwarded_text = String::new();

    loop {
        let next_chunk = if let Some(token) = cancellation_token {
            tokio::select! {
                () = token.cancelled() => {
                    // Cancel after visible streamed text: persist-worthy,
                    // exactly like the pre-consolidation engine's
                    // committed-partial-on-cancel.
                    if forwarded_text.is_empty() {
                        return Err(ToolLoopCancelled.into());
                    }
                    return Err(StreamCancelledAfterOutput::new(forwarded_text).into());
                }
                chunk = provider_stream.next() => chunk,
            }
        } else {
            provider_stream.next().await
        };

        let Some(event_result) = next_chunk else {
            break;
        };

        let event = match event_result {
            Ok(event) => event,
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                    "model_provider stream emitted an error event"
                );
                let message = format!("model_provider stream error: {err}");
                if visible_event_output {
                    // Persist only what the consumer actually saw
                    // (`forwarded_text`), never the raw accumulated text —
                    // that includes guard-withheld protocol fragments and
                    // suppression-buffered output nobody received.
                    return Err(StreamInterruptedAfterOutput {
                        partial_text: forwarded_text,
                        message,
                    }
                    .into());
                }
                return Err(anyhow::Error::msg(message));
            }
        };
        match event {
            StreamEvent::Final => break,
            StreamEvent::Usage(usage) => {
                outcome.usage = Some(usage);
            }
            StreamEvent::ToolCall(tool_call) => {
                outcome.tool_calls.push(tool_call);
                suppress_forwarding = true;
                text_guard.suppress_forwarding = true;
            }
            // Pre-executed tool events are for observability only: they are
            // relayed as TurnEvents but do not affect the agent's tool
            // dispatch loop.
            StreamEvent::PreExecutedToolCall { name, args } => {
                let id = Uuid::new_v4().to_string();
                pre_executed_ids
                    .entry(name.clone())
                    .or_default()
                    .push_back(id.clone());
                if let Some(tx) = event_tx {
                    visible_event_output = true;
                    let _ = tx
                        .send(TurnEvent::ToolCall {
                            id,
                            name,
                            args: serde_json::from_str(&args).unwrap_or(serde_json::Value::Null),
                        })
                        .await;
                }
            }
            StreamEvent::PreExecutedToolResult { name, output } => {
                let id = pre_executed_ids
                    .get_mut(&name)
                    .and_then(|ids| ids.pop_front())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                if let Some(tx) = event_tx {
                    visible_event_output = true;
                    let _ = tx.send(TurnEvent::ToolResult { id, name, output }).await;
                }
            }
            StreamEvent::TextDelta(chunk) => {
                // Reasoning/thinking deltas arrive on the same `TextDelta`
                // event as plain text but populate `chunk.reasoning` instead
                // of `chunk.delta`. They must be captured into the outcome
                // even when `chunk.delta` is empty — otherwise model_providers
                // that require reasoning to round-trip on subsequent turns
                // (DeepSeek V4 thinking mode; see #6059) reject the next
                // request with a 400. Reasoning is never forwarded as a
                // visible response delta — it is the model's internal
                // monologue, kept for replay only.
                if let Some(reasoning) = chunk.reasoning.as_deref()
                    && !reasoning.is_empty()
                {
                    outcome.reasoning_content.push_str(reasoning);
                    // Thinking is surfaced as its own TurnEvent variant; it
                    // must never reach the Chunk/draft text surfaces.
                    if let Some(tx) = event_tx {
                        visible_event_output = true;
                        let _ = tx
                            .send(TurnEvent::Thinking {
                                delta: reasoning.to_string(),
                            })
                            .await;
                    }
                }

                if chunk.delta.is_empty() {
                    continue;
                }

                let sanitized_delta = think_stripper.push(&chunk.delta);
                if sanitized_delta.is_empty() {
                    continue;
                }

                outcome.response_text.push_str(&sanitized_delta);

                if suppress_forwarding {
                    continue;
                }

                if strict_tool_parsing {
                    // Every event_tx send is gated on event_tx ALONE — never
                    // nested under on_delta. Nesting doubles chunks when both
                    // are Some and drops them when on_delta is None.
                    if let Some(tx) = event_tx {
                        outcome.forwarded_live_deltas = true;
                        visible_event_output = true;
                        forwarded_text.push_str(&sanitized_delta);
                        let _ = tx
                            .send(TurnEvent::Chunk {
                                delta: sanitized_delta.clone(),
                            })
                            .await;
                    }
                    if let Some(tx) = delta_sender {
                        outcome.forwarded_live_deltas = true;
                        if tx.send(StreamDelta::Text(sanitized_delta)).await.is_err() {
                            delta_sender = None;
                        }
                    }
                    continue;
                }

                let Some(forward_text) = text_guard.push(&sanitized_delta) else {
                    continue;
                };

                if let Some(tx) = event_tx {
                    outcome.forwarded_live_deltas = true;
                    visible_event_output = true;
                    forwarded_text.push_str(&forward_text);
                    let _ = tx
                        .send(TurnEvent::Chunk {
                            delta: forward_text.clone(),
                        })
                        .await;
                }
                if let Some(tx) = delta_sender {
                    outcome.forwarded_live_deltas = true;
                    if tx.send(StreamDelta::Text(forward_text)).await.is_err() {
                        delta_sender = None;
                    }
                }
            }
        }
    }

    let trailing_delta = think_stripper.finish();
    if !trailing_delta.is_empty() {
        outcome.response_text.push_str(&trailing_delta);
        if !suppress_forwarding {
            if strict_tool_parsing {
                if let Some(tx) = event_tx {
                    outcome.forwarded_live_deltas = true;
                    let _ = tx
                        .send(TurnEvent::Chunk {
                            delta: trailing_delta.clone(),
                        })
                        .await;
                }
                if let Some(tx) = delta_sender {
                    outcome.forwarded_live_deltas = true;
                    if tx.send(StreamDelta::Text(trailing_delta)).await.is_err() {
                        delta_sender = None;
                    }
                }
            } else if let Some(forward_text) = text_guard.push(&trailing_delta) {
                if let Some(tx) = event_tx {
                    outcome.forwarded_live_deltas = true;
                    let _ = tx
                        .send(TurnEvent::Chunk {
                            delta: forward_text.clone(),
                        })
                        .await;
                }
                if let Some(tx) = delta_sender {
                    outcome.forwarded_live_deltas = true;
                    if tx.send(StreamDelta::Text(forward_text)).await.is_err() {
                        delta_sender = None;
                    }
                }
            }
        }
    }

    if let Some(forward_text) = text_guard.finish() {
        if let Some(tx) = event_tx {
            outcome.forwarded_live_deltas = true;
            let _ = tx
                .send(TurnEvent::Chunk {
                    delta: forward_text.clone(),
                })
                .await;
        }
        if let Some(tx) = delta_sender {
            outcome.forwarded_live_deltas = true;
            let _ = tx.send(StreamDelta::Text(forward_text)).await;
        }
    }
    outcome.suppressed_protocol = text_guard.suppressed_protocol;

    Ok(outcome)
}
