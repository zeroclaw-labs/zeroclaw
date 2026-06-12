//! Stream/draft event types and pacing constants for the turn loop, plus the
//! loop's `TurnEvent` emission helpers (#7415 consolidation).

use super::outcome::ToolLoopCancelled;
use crate::agent::tool_execution::ToolExecutionOutcome;
use anyhow::Result;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_tool_call_parser::ParsedToolCall;

/// Minimum characters per chunk when relaying LLM text to a streaming draft.
pub(crate) const STREAM_CHUNK_MIN_CHARS: usize = 80;

/// Minimum interval between progress sends to avoid flooding the draft channel.
pub const PROGRESS_MIN_INTERVAL_MS: u64 = 500;

/// Delta sent from the agent loop to the channel's draft updater.
/// Append-only — no clear/reset variant exists by design.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// Response text to append to the message buffer.
    Text(String),
    /// Ephemeral tool progress (not part of the response body).
    Status(String),
}

/// Backwards-compatible alias while callers are migrated.
pub type DraftEvent = StreamDelta;

/// Send `text` to the draft channel in word-aligned chunks of at least
/// [`STREAM_CHUNK_MIN_CHARS`] (upstream loop body, no-tool-calls final exit).
/// Used when the final response wasn't already streamed live. Honors the
/// cancellation token between chunks; a closed receiver stops chunking
/// silently.
pub(crate) async fn stream_text_posthoc_chunks(
    on_delta: &Sender<DraftEvent>,
    text: &str,
    cancellation_token: Option<&CancellationToken>,
) -> Result<()> {
    let mut chunk = String::new();
    for word in text.split_inclusive(char::is_whitespace) {
        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            return Err(ToolLoopCancelled.into());
        }
        chunk.push_str(word);
        if chunk.len() >= STREAM_CHUNK_MIN_CHARS
            && on_delta
                .send(StreamDelta::Text(std::mem::take(&mut chunk)))
                .await
                .is_err()
        {
            break;
        }
    }
    if !chunk.is_empty() {
        let _ = on_delta.send(StreamDelta::Text(chunk)).await;
    }
    Ok(())
}

/// Emit the `TurnEvent::ToolCall`/`ToolResult` pair for one executed tool
/// call (upstream E2 parity: per-outcome emission after execution).
///
/// Text-protocol parsed calls carry `tool_call_id: None`; the TurnEvent
/// contract documents `id` as a stable correlation id shared by the pair
/// (ACP/WS clients key on it), so a fresh UUID is synthesized per pair —
/// the old streaming engine pre-assigned UUIDs to id-less calls for the
/// same reason. Distinct calls in one batch must never collapse to `""`.
pub(crate) async fn emit_tool_call_pair(
    event_tx: &Sender<TurnEvent>,
    call: &ParsedToolCall,
    outcome: &ToolExecutionOutcome,
) {
    let call_id = call
        .tool_call_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let _ = event_tx
        .send(TurnEvent::ToolCall {
            id: call_id.clone(),
            name: call.name.clone(),
            args: call.arguments.clone(),
        })
        .await;
    let _ = event_tx
        .send(TurnEvent::ToolResult {
            id: call_id,
            name: call.name.clone(),
            output: outcome.output.clone(),
        })
        .await;
}

/// `TurnEvent` variant of [`stream_text_posthoc_chunks`]: when the final
/// response was not streamed live, emit it as one post-hoc `Chunk`.
pub(crate) async fn emit_posthoc_turn_chunk(event_tx: Option<&Sender<TurnEvent>>, text: &str) {
    if let Some(tx) = event_tx {
        let _ = tx
            .send(TurnEvent::Chunk {
                delta: text.to_string(),
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn parsed_call(id: Option<&str>) -> ParsedToolCall {
        ParsedToolCall {
            name: "echo".into(),
            arguments: serde_json::json!({}),
            tool_call_id: id.map(str::to_string),
        }
    }

    fn ok_outcome() -> ToolExecutionOutcome {
        ToolExecutionOutcome {
            output: "out".into(),
            success: true,
            error_reason: None,
            duration: Duration::ZERO,
            receipt: None,
        }
    }

    /// Text-protocol calls have no id; the pair must still correlate via a
    /// fresh non-empty id, and two id-less calls must never share one.
    #[tokio::test]
    async fn idless_calls_get_distinct_synthesized_pair_ids() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        emit_tool_call_pair(&tx, &parsed_call(None), &ok_outcome()).await;
        emit_tool_call_pair(&tx, &parsed_call(None), &ok_outcome()).await;
        drop(tx);

        let mut ids = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::ToolCall { id, .. } | TurnEvent::ToolResult { id, .. } => ids.push(id),
                _ => {}
            }
        }
        assert_eq!(ids.len(), 4, "two pairs = four events");
        assert!(
            ids.iter().all(|id| !id.is_empty()),
            "synthesized ids must be non-empty: {ids:?}"
        );
        assert_eq!(
            ids[0], ids[1],
            "ToolCall/ToolResult of one pair must share the id"
        );
        assert_eq!(ids[2], ids[3], "second pair must share its id");
        assert_ne!(ids[0], ids[2], "distinct calls must get distinct ids");
    }

    /// Parser-assigned ids pass through untouched.
    #[tokio::test]
    async fn existing_ids_pass_through() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        emit_tool_call_pair(&tx, &parsed_call(Some("native-7")), &ok_outcome()).await;
        drop(tx);
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::ToolCall { id, .. } | TurnEvent::ToolResult { id, .. } => {
                    assert_eq!(id, "native-7");
                }
                _ => {}
            }
        }
    }
}
