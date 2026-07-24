//! Stream/draft event types and pacing constants for the turn loop, plus the
//! loop's `TurnEvent` emission helpersconsolidation).

use super::outcome::ToolLoopCancelled;
use super::redact::scrub_credentials;
use crate::agent::tool_execution::ToolExecutionOutcome;
use anyhow::Result;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::attribution::Role;
use zeroclaw_tool_call_parser::ParsedToolCall;

/// Minimum characters per chunk when relaying live model text to draft surfaces.
pub(crate) const STREAM_CHUNK_MIN_CHARS: usize = 80;

/// Minimum interval between progress sends to avoid flooding the draft channel.
pub const PROGRESS_MIN_INTERVAL_MS: u64 = 500;

/// Placeholder text used for newly opened draft messages.
pub const DRAFT_PLACEHOLDER: &str = "...";
/// Prefix for liveness-only thinking/reasoning progress.
pub const THINKING_STATUS_PREFIX: &str = "\u{1f914} ";
/// Prefix for opt-in raw reasoning progress.
pub const REASONING_FULL_PREFIX: &str = THINKING_STATUS_PREFIX;
const THINKING_STATUS_LABEL: &str = "Thinking...";
const THINKING_STATUS_ROUND_PREFIX: &str = "Thinking (round ";
const THINKING_STATUS_ROUND_SUFFIX: &str = ")...";

/// Status-mode reasoning tick that does not expose raw reasoning text.
pub fn thinking_status_text(iteration: usize) -> String {
    let round = iteration + 1;
    if round == 1 {
        format!("{THINKING_STATUS_PREFIX}{THINKING_STATUS_LABEL}\n")
    } else {
        format!(
            "{THINKING_STATUS_PREFIX}{THINKING_STATUS_ROUND_PREFIX}{round}{THINKING_STATUS_ROUND_SUFFIX}\n"
        )
    }
}

/// Parse the label portion of a generated status-mode reasoning line.
pub fn thinking_status_label_round(label: &str) -> Option<usize> {
    if label == THINKING_STATUS_LABEL {
        return Some(1);
    }
    label
        .strip_prefix(THINKING_STATUS_ROUND_PREFIX)
        .and_then(|rest| rest.strip_suffix(THINKING_STATUS_ROUND_SUFFIX))
        .and_then(|round| round.parse::<usize>().ok())
        .filter(|round| *round > 1)
}

/// Comparable round number for a generated status-mode reasoning line.
pub fn thinking_status_round(text: &str) -> Option<usize> {
    let label = text
        .strip_prefix(THINKING_STATUS_PREFIX)?
        .strip_suffix('\n')?;
    thinking_status_label_round(label)
}

/// Whether a progress line is one of the liveness-only thinking status lines.
pub fn is_thinking_status_text(text: &str) -> bool {
    thinking_status_round(text).is_some()
}

/// Delta sent from the agent loop to the channel's draft updater.
/// Append-only — no clear/reset variant exists by design.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// Response text to append to the message buffer.
    Text(String),
    /// Ephemeral tool progress (not part of the response body).
    Status(String),
    /// A pending tool call. Channel draft consumers decide how to render its
    /// arguments; the runtime keeps this event structured to avoid coupling a
    /// transport-specific disclosure policy into the agent loop.
    ToolStart {
        tool: String,
        arguments: std::sync::Arc<serde_json::Value>,
        /// Canonical attribution carried from the resolved tool. `None`
        /// means the name did not resolve in the static tool registry.
        tool_role: Option<Role>,
    },
    /// A completed tool call paired with its original arguments.
    ToolComplete {
        tool: String,
        arguments: std::sync::Arc<serde_json::Value>,
        /// The same attribution observed when the matching start event was
        /// emitted; consumers must treat `None` as untrusted.
        tool_role: Option<Role>,
        secs: u64,
        success: bool,
        error: Option<String>,
    },
    /// Provider reasoning text. Channel surfaces must opt in before rendering.
    Reasoning(String),
}

impl StreamDelta {
    /// Render structured tool events with the historical conservative policy.
    /// Non-Matrix consumers must use this instead of serializing arguments.
    #[must_use]
    pub fn legacy_status(&self) -> Option<String> {
        match self {
            Self::ToolStart {
                tool, arguments, ..
            } => Some(super::progress::render_tool_start_progress(tool, arguments)),
            Self::ToolComplete {
                tool,
                arguments,
                secs,
                success,
                error,
                ..
            } => Some(super::progress::render_tool_completion_progress(
                tool,
                arguments,
                *secs,
                *success,
                error.as_deref(),
            )),
            Self::Text(_) | Self::Status(_) | Self::Reasoning(_) => None,
        }
    }
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

    #[test]
    fn thinking_status_parser_requires_exact_generated_status_text() {
        assert!(is_thinking_status_text(&thinking_status_text(0)));
        assert_eq!(thinking_status_round(&thinking_status_text(0)), Some(1));
        assert!(is_thinking_status_text(&thinking_status_text(1)));
        assert_eq!(thinking_status_round(&thinking_status_text(1)), Some(2));
        assert!(!is_thinking_status_text(&format!(
            "{REASONING_FULL_PREFIX}Thinking (round 2) through the next option"
        )));
        assert!(!is_thinking_status_text(&format!(
            "{THINKING_STATUS_PREFIX}Thinking (round 1)...\n"
        )));
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
