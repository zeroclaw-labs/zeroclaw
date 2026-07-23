//! Stream/draft event types and pacing constants for the turn loop, plus the
//! loop's `TurnEvent` emission helpersconsolidation).

use super::outcome::ToolLoopCancelled;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use anyhow::Result;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::{ToolArtifact, TurnEvent};
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

pub(crate) fn resolve_tool_call_id(call: &ParsedToolCall) -> String {
    call.tool_call_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

pub(crate) async fn emit_tool_call_pending(
    event_tx: &Sender<TurnEvent>,
    id: &str,
    call: &ParsedToolCall,
) {
    let _ = event_tx
        .send(TurnEvent::ToolCall {
            id: id.to_string(),
            name: call.name.clone(),
            args: call.arguments.clone(),
        })
        .await;
}

/// Emit the `TurnEvent::ToolResult` that completes a previously-pending call.
/// `id` must match the [`emit_tool_call_pending`] that opened the card.
pub(crate) async fn emit_tool_result(
    event_tx: &Sender<TurnEvent>,
    id: &str,
    name: &str,
    outcome: &ToolExecutionOutcome,
) {
    let _ = event_tx
        .send(TurnEvent::ToolResult {
            id: id.to_string(),
            name: name.to_string(),
            output: scrub_credentials(&outcome.output),
            // Project the tool's structured output into typed artifact metadata
            // when it declared a delivered file, so channels never parse `output`.
            artifact: outcome
                .output_data
                .as_ref()
                .and_then(ToolArtifact::from_delivered_data),
        })
        .await;
}

/// Emit a pending `ToolCall` immediately followed by its `ToolResult` for a
/// call that never reached execution (hook-cancelled, denied, replaced,
/// deduplicated). These have no live window between the two halves, so a
/// single resolved id keeps the pair correlated without a pre-exec emit.
pub(crate) async fn emit_tool_call_pair(
    event_tx: &Sender<TurnEvent>,
    call: &ParsedToolCall,
    outcome: &ToolExecutionOutcome,
) {
    let call_id = resolve_tool_call_id(call);
    emit_tool_call_pending(event_tx, &call_id, call).await;
    emit_tool_result(event_tx, &call_id, &call.name, outcome).await;
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
            output_data: None,
        }
    }

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

    #[tokio::test]
    async fn split_pending_then_result_share_resolved_id() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let call = parsed_call(None);
        let id = resolve_tool_call_id(&call);
        emit_tool_call_pending(&tx, &id, &call).await;
        emit_tool_result(&tx, &id, &call.name, &ok_outcome()).await;
        drop(tx);

        let pending = rx.recv().await.expect("pending event");
        let result = rx.recv().await.expect("result event");
        let pending_id = match pending {
            TurnEvent::ToolCall { id, .. } => id,
            other => panic!("expected ToolCall first, got {other:?}"),
        };
        let result_id = match result {
            TurnEvent::ToolResult { id, .. } => id,
            other => panic!("expected ToolResult second, got {other:?}"),
        };
        assert!(!pending_id.is_empty(), "resolved id must be non-empty");
        assert_eq!(
            pending_id, result_id,
            "pending card and its result must share the id"
        );
    }

    #[tokio::test]
    async fn tool_result_event_is_scrubbed_for_rendering() {
        let outcome = ToolExecutionOutcome {
            output: "api_key = \"sk-live-abcd1234efgh5678\"".into(),
            success: true,
            error_reason: None,
            duration: Duration::ZERO,
            receipt: None,
            output_data: None,
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        emit_tool_call_pair(&tx, &parsed_call(Some("c1")), &outcome).await;
        drop(tx);
        let mut saw_result = false;
        while let Some(ev) = rx.recv().await {
            if let TurnEvent::ToolResult { output, .. } = ev {
                saw_result = true;
                assert!(output.contains("[REDACTED]"));
                assert!(!output.contains("abcd1234efgh5678"));
            }
        }
        assert!(saw_result, "a ToolResult event must be emitted");
    }
}
