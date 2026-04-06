use crate::providers::traits::StreamEvent;
use crate::providers::{ChatMessage, ChatRequest, Provider, ToolCall};
use crate::tools::ToolSpec;
use anyhow::Result;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use super::loop_::ToolLoopCancelled;

/// Minimum characters per chunk when relaying LLM text to a streaming draft.
pub(crate) const STREAM_CHUNK_MIN_CHARS: usize = 80;
/// Rolling window size for detecting streamed tool-call payload markers.
const STREAM_TOOL_MARKER_WINDOW_CHARS: usize = 512;

/// Minimum interval between progress sends to avoid flooding the draft channel.
pub(crate) const PROGRESS_MIN_INTERVAL_MS: u64 = 500;

/// Structured event sent through the draft channel so channels can
/// differentiate between status/progress updates and actual response content.
#[derive(Debug, Clone)]
pub(crate) enum DraftEvent {
    /// Clear accumulated draft content (e.g. before streaming a new response).
    Clear,
    /// Progress / status text — channels can show this in a status bar
    /// rather than in the message body (e.g. "🤔 Thinking...", "⏳ shell_command").
    Progress(String),
    /// Actual response content delta to append to the draft message.
    Content(String),
}

#[derive(Debug, Default)]
pub(crate) struct StreamedChatOutcome {
    pub response_text: String,
    pub tool_calls: Vec<ToolCall>,
    pub forwarded_live_deltas: bool,
}

pub(crate) async fn consume_provider_streaming_response(
    provider: &dyn Provider,
    messages: &[ChatMessage],
    request_tools: Option<&[ToolSpec]>,
    model: &str,
    temperature: f64,
    cancellation_token: Option<&CancellationToken>,
    on_delta: Option<&tokio::sync::mpsc::Sender<DraftEvent>>,
) -> Result<StreamedChatOutcome> {
    let mut provider_stream = provider.stream_chat(
        ChatRequest {
            messages,
            tools: request_tools,
        },
        model,
        temperature,
        crate::providers::traits::StreamOptions::new(true),
    );
    let mut outcome = StreamedChatOutcome::default();
    let mut delta_sender = on_delta;
    let mut suppress_forwarding = false;
    let mut marker_window = String::new();

    loop {
        let next_chunk = if let Some(token) = cancellation_token {
            tokio::select! {
                () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                chunk = provider_stream.next() => chunk,
            }
        } else {
            provider_stream.next().await
        };

        let Some(event_result) = next_chunk else {
            break;
        };

        let event = event_result.map_err(|err| anyhow::anyhow!("provider stream error: {err}"))?;
        match event {
            StreamEvent::Final => break,
            StreamEvent::ToolCall(tool_call) => {
                outcome.tool_calls.push(tool_call);
                suppress_forwarding = true;
                if outcome.forwarded_live_deltas {
                    if let Some(tx) = delta_sender {
                        let _ = tx.send(DraftEvent::Clear).await;
                    }
                    outcome.forwarded_live_deltas = false;
                }
            }
            StreamEvent::PreExecutedToolCall { .. } | StreamEvent::PreExecutedToolResult { .. } => {
                // Pre-executed tool events are for observability only.
                // They are forwarded to the gateway via turn_streamed but
                // do not affect the agent's tool dispatch loop.
            }
            StreamEvent::TextDelta(chunk) => {
                if chunk.delta.is_empty() {
                    continue;
                }

                outcome.response_text.push_str(&chunk.delta);
                marker_window.push_str(&chunk.delta);

                if marker_window.len() > STREAM_TOOL_MARKER_WINDOW_CHARS {
                    let keep_from = marker_window.len() - STREAM_TOOL_MARKER_WINDOW_CHARS;
                    let boundary = marker_window
                        .char_indices()
                        .find(|(idx, _)| *idx >= keep_from)
                        .map_or(0, |(idx, _)| idx);
                    marker_window.drain(..boundary);
                }

                if !suppress_forwarding && {
                    let lowered = marker_window.to_ascii_lowercase();
                    lowered.contains("<tool_call")
                        || lowered.contains("<toolcall")
                        || lowered.contains("\"tool_calls\"")
                } {
                    suppress_forwarding = true;
                    if outcome.forwarded_live_deltas {
                        if let Some(tx) = delta_sender {
                            let _ = tx.send(DraftEvent::Clear).await;
                        }
                        outcome.forwarded_live_deltas = false;
                    }
                }

                if suppress_forwarding {
                    continue;
                }

                if let Some(tx) = delta_sender {
                    if !outcome.forwarded_live_deltas {
                        let _ = tx.send(DraftEvent::Clear).await;
                        outcome.forwarded_live_deltas = true;
                    }
                    if tx.send(DraftEvent::Content(chunk.delta)).await.is_err() {
                        delta_sender = None;
                    }
                }
            }
        }
    }

    Ok(outcome)
}
