//! Shared turn execution. Single source of truth for spawn-drain-cancel.

use crate::agent::agent::{Agent, StreamedTurnError, StreamedTurnSuccess, TurnEvent};
use crate::agent::loop_::is_tool_loop_cancelled;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::model_provider::ConversationMessage;

pub enum TurnOutcome {
    Completed {
        text: String,
        messages: Vec<ConversationMessage>,
    },
    Cancelled {
        partial_text: String,
        messages: Vec<ConversationMessage>,
    },
}

#[derive(Debug)]
pub enum TurnError {
    Panicked(String),
    AgentError(String),
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Panicked(msg) => write!(f, "Turn task panicked: {msg}"),
            Self::AgentError(msg) => write!(f, "Agent turn failed: {msg}"),
        }
    }
}

impl std::error::Error for TurnError {}

/// Attribution fields attached to the tracing span for the duration of a turn.
/// All fields appear on every `record!()` emitted inside the turn.
#[derive(Clone, Default)]
pub struct TurnAttribution {
    pub session_key: Option<String>,
    pub agent_alias: String,
    pub model_provider: String,
    pub model: String,
    pub channel: &'static str,
}

pub async fn execute_turn<F, Fut>(
    agent: Arc<Mutex<Agent>>,
    prompt: String,
    cancel: CancellationToken,
    attribution: TurnAttribution,
    on_event: F,
) -> Result<TurnOutcome, TurnError>
where
    F: Fn(TurnEvent) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let (event_tx, mut event_rx) = mpsc::channel::<TurnEvent>(64);
    let cancel_clone = cancel.clone();
    let session_key = attribution.session_key.clone();

    let mut turn_handle = zeroclaw_spawn::spawn!(async move {
        let mut guard = agent.lock().await;
        let sk = attribution.session_key.clone();
        crate::agent::loop_::scope_session_key(attribution.session_key, async move {
            use ::zeroclaw_log::Instrument as _;
            let span = ::zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "zeroclaw_scope",
                session_key = %sk.as_deref().unwrap_or(""),
                agent_alias = %attribution.agent_alias,
                model_provider = %attribution.model_provider,
                model = %attribution.model,
                channel = %attribution.channel,
            );
            guard
                .turn_streamed_with_steering_state(&prompt, event_tx, Some(cancel_clone), None)
                .instrument(span)
                .await
        })
        .await
    });

    let mut accumulated_text = String::new();

    // Drive the turn by draining its event channel, but never let a turn task
    // wedged inside a non-cancellable tool call (shell, HTTP, a stalled provider
    // stream) hold the dispatch path hostage. The drain exits on channel close,
    // explicit cancel, OR an idle-stall bound; the latter two return Cancelled
    // and the in-flight task is aborted on drop.
    let drain =
        drain_until_done_or_cancelled(&mut event_rx, &cancel, &mut accumulated_text, &on_event)
            .await;
    let _ = session_key; // consumed above

    match drain {
        DrainOutcome::Completed => {
            let joined = turn_handle
                .await
                .map_err(|e| TurnError::Panicked(format!("{e}")))?;
            outcome_from_task_result(joined, accumulated_text)
        }
        DrainOutcome::ExplicitCancel => {
            // The turn task races the same cancel token and unwinds
            // cooperatively: it synthesizes results for any in-flight tool
            // call, pushes the `[interrupted]` assistant message, and commits
            // both into the agent history before returning. Persistence reads
            // that committed history, so aborting the task mid-commit drops
            // the cancelled turn's tool exchange and corrupts the next turn.
            // Give the task a bounded grace window to land its own unwind;
            // only abort if it is genuinely wedged in a non-cooperative call.
            match tokio::time::timeout(CANCEL_GRACE, &mut turn_handle).await {
                Ok(joined) => outcome_from_task_result(
                    joined.map_err(|e| TurnError::Panicked(format!("{e}")))?,
                    accumulated_text,
                ),
                Err(_) => {
                    turn_handle.abort();
                    Ok(TurnOutcome::Cancelled {
                        partial_text: accumulated_text,
                        messages: Vec::new(),
                    })
                }
            }
        }
    }
}

/// Grace window allowing a cancelled turn task to commit its cooperative
/// unwind (synthesized tool results + `[interrupted]` message) into the agent
/// history before the dispatch path falls back to a hard abort.
const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Map a finished turn task into a [`TurnOutcome`]. A successful turn yields
/// `Completed`; a cooperative cancel yields `Cancelled` carrying the messages
/// the task committed so persistence never depends on the abort/commit race.
fn outcome_from_task_result(
    joined: Result<StreamedTurnSuccess, StreamedTurnError>,
    accumulated_text: String,
) -> Result<TurnOutcome, TurnError> {
    match joined {
        Ok(StreamedTurnSuccess {
            response,
            new_messages,
        }) => Ok(TurnOutcome::Completed {
            text: response,
            messages: new_messages,
        }),
        Err(StreamedTurnError {
            error,
            committed_response,
            new_messages,
        }) if is_tool_loop_cancelled(&error) => Ok(TurnOutcome::Cancelled {
            partial_text: if committed_response.is_empty() {
                accumulated_text
            } else {
                committed_response
            },
            messages: new_messages,
        }),
        Err(StreamedTurnError { error, .. }) => Err(TurnError::AgentError(format!("{error}"))),
    }
}

/// Why [`drain_until_done_or_cancelled`] returned. `ExplicitCancel` is an
/// outside fire (client RPC, reaper, session removal) that reached the drain.
/// There is no self-firing idle exit: a live turn falls silent for the whole
/// duration of a tool call, so silence is never treated as a stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainOutcome {
    Completed,
    ExplicitCancel,
}

/// Drain `event_rx` until the turn finishes or the cancel token fires. Chunk
/// deltas accumulate in `accumulated` so partial text survives a cancel. The
/// only terminals are the turn task dropping its sender (`recv` -> `None`,
/// [`DrainOutcome::Completed`]) and an explicit cancel
/// ([`DrainOutcome::ExplicitCancel`]). A wedged turn is bounded by the explicit
/// layers — ownership-gated `session/cancel` and the reaper — never by guessing
/// from channel quiet.
async fn drain_until_done_or_cancelled<F, Fut>(
    event_rx: &mut mpsc::Receiver<TurnEvent>,
    cancel: &CancellationToken,
    accumulated: &mut String,
    on_event: &F,
) -> DrainOutcome
where
    F: Fn(TurnEvent) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    loop {
        if cancel.is_cancelled() {
            return DrainOutcome::ExplicitCancel;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return DrainOutcome::ExplicitCancel,
            maybe_event = event_rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        if let TurnEvent::Chunk { ref delta } = event {
                            accumulated.push_str(delta);
                        }
                        on_event(event).await;
                    }
                    None => return DrainOutcome::Completed,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn noop(_e: TurnEvent) -> std::future::Ready<()> {
        std::future::ready(())
    }

    #[tokio::test]
    async fn drain_must_not_idle_cancel_a_live_turn_across_a_long_tool_gap() {
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(8);
        let cancel = CancellationToken::new();
        let mut acc = String::new();

        let sender = zeroclaw_spawn::spawn!(async move {
            let _ = tx
                .send(TurnEvent::ToolCall {
                    id: "c1".to_string(),
                    name: "shell".to_string(),
                    args: serde_json::json!({ "command": "cargo test" }),
                })
                .await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let _ = tx
                .send(TurnEvent::ToolResult {
                    id: "c1".to_string(),
                    name: "shell".to_string(),
                    output: "ok".to_string(),
                })
                .await;
            let _ = tx
                .send(TurnEvent::Chunk {
                    delta: "done".to_string(),
                })
                .await;
        });

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            drain_until_done_or_cancelled(&mut rx, &cancel, &mut acc, &noop),
        )
        .await
        .expect("drain must terminate when the live turn task completes");

        sender.await.unwrap();
        assert_eq!(
            outcome,
            DrainOutcome::Completed,
            "a turn whose sender is alive but quiet during a long tool \
             execution is NOT stalled; silence during execute_tools is the \
             normal case. Killing it is the idle_stall regression that froze \
             the TUI mid-turn (sessions 102, 103)."
        );
        assert!(
            !cancel.is_cancelled(),
            "drain self-cancelled a healthy turn across a tool gap; the token \
             must stay clean so downstream records no cancel."
        );
        assert_eq!(
            acc, "done",
            "drain dropped the post-tool chunk after wrongly tripping an idle \
             bound mid-execution."
        );
    }

    #[tokio::test]
    async fn drain_must_still_accumulate_chunks_when_events_arrive_steadily() {
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(8);
        let cancel = CancellationToken::new();
        let mut acc = String::new();

        let sender = zeroclaw_spawn::spawn!(async move {
            for delta in ["he", "llo", " ", "world"] {
                let _ = tx
                    .send(TurnEvent::Chunk {
                        delta: delta.to_string(),
                    })
                    .await;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        });

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            drain_until_done_or_cancelled(&mut rx, &cancel, &mut acc, &noop),
        )
        .await
        .expect("drain must terminate after the sender drops");

        sender.await.unwrap();
        assert_eq!(
            cancelled,
            DrainOutcome::Completed,
            "channel closure is not a cancel; drain returned the wrong verdict"
        );
        assert_eq!(
            acc, "hello world",
            "drain dropped chunks instead of accumulating them; a fix that \
             short-circuits with too-aggressive an idle window (e.g. <250ms) \
             would corrupt legitimate streaming turns. The production idle \
             window must sit comfortably between the inter-chunk gap of a \
             healthy stream (~hundreds of ms) and the user-perceptible hang \
             threshold (~seconds)."
        );
    }

    #[test]
    fn cancel_outcome_carries_committed_messages_not_just_partial_text() {
        // A cooperative cancel returns StreamedTurnError whose new_messages
        // hold the synthesized tool results + `[interrupted]` message the task
        // already committed. The mapping must surface them, not drop them onto
        // the floor and fall back to bare accumulated text — that drop is what
        // truncated the cancelled turn's tool exchange from persisted history.
        let msgs = vec![ConversationMessage::Chat(
            zeroclaw_providers::ChatMessage::assistant("[interrupted by user]"),
        )];
        let err = StreamedTurnError {
            error: crate::agent::loop_::ToolLoopCancelled.into(),
            committed_response: "partial".to_string(),
            new_messages: msgs.clone(),
        };

        let outcome = outcome_from_task_result(Err(err), "accumulated".to_string())
            .expect("cooperative cancel maps to a Cancelled outcome, not an error");

        match outcome {
            TurnOutcome::Cancelled {
                partial_text,
                messages,
            } => {
                assert_eq!(
                    partial_text, "partial",
                    "committed_response from the task must win over the drain's \
                     accumulated text when present"
                );
                assert_eq!(
                    messages.len(),
                    msgs.len(),
                    "cancelled outcome dropped the messages the task committed"
                );
            }
            TurnOutcome::Completed { .. } => {
                panic!("a tool-loop cancel must not map to Completed")
            }
        }
    }

    #[test]
    fn non_cancel_agent_error_stays_an_error() {
        let err = StreamedTurnError {
            error: anyhow::Error::msg("provider exploded"),
            committed_response: String::new(),
            new_messages: Vec::new(),
        };
        let outcome = outcome_from_task_result(Err(err), String::new());
        assert!(
            matches!(outcome, Err(TurnError::AgentError(_))),
            "a genuine agent failure must surface as an error, not a silent \
             cancel"
        );
    }
}
