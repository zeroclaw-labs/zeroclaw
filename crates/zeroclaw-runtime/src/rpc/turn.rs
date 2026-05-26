//! Shared turn execution. Single source of truth for spawn-drain-cancel.

use crate::agent::agent::{Agent, TurnEvent};
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

pub async fn execute_turn<F, Fut>(
    agent: Arc<Mutex<Agent>>,
    prompt: String,
    cancel: CancellationToken,
    session_key: Option<String>,
    on_event: F,
) -> Result<TurnOutcome, TurnError>
where
    F: Fn(TurnEvent) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let (event_tx, mut event_rx) = mpsc::channel::<TurnEvent>(64);
    let cancel_clone = cancel.clone();

    let turn_handle = zeroclaw_spawn::spawn!(async move {
        let mut guard = agent.lock().await;
        let session_key_for_scope = session_key.clone();
        crate::agent::loop_::scope_session_key(session_key, async move {
            use ::zeroclaw_log::Instrument as _;
            let span = ::zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "zeroclaw_scope",
                session_key = %session_key_for_scope.as_deref().unwrap_or(""),
            );
            guard
                .turn_streamed(&prompt, event_tx, Some(cancel_clone))
                .instrument(span)
                .await
        })
        .await
    });

    let mut accumulated_text = String::new();
    while let Some(event) = event_rx.recv().await {
        if let TurnEvent::Chunk { ref delta } = event {
            accumulated_text.push_str(delta);
        }
        on_event(event).await;
    }

    match turn_handle
        .await
        .map_err(|e| TurnError::Panicked(format!("{e}")))?
    {
        Ok((text, messages)) => Ok(TurnOutcome::Completed { text, messages }),
        Err(e) if is_tool_loop_cancelled(&e) => Ok(TurnOutcome::Cancelled {
            partial_text: accumulated_text,
        }),
        Err(e) => Err(TurnError::AgentError(format!("{e}"))),
    }
}
