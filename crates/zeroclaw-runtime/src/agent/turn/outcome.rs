//! Turn-loop control-flow outcomes: cancellation and model-switch errors.

use std::sync::{Arc, Mutex};

/// Callback type for checking if model has been switched during tool execution.
/// Returns Some((model_provider, model)) if a switch was requested, None otherwise.
pub type ModelSwitchCallback = Arc<Mutex<Option<(String, String)>>>;

#[derive(Debug)]
pub struct ToolLoopCancelled;

impl std::fmt::Display for ToolLoopCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled")
    }
}

impl std::error::Error for ToolLoopCancelled {}

pub fn is_tool_loop_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(|source| source.is::<ToolLoopCancelled>())
}

/// A provider stream failed *after* caller-visible output (text chunks,
/// thinking, pre-executed tool events) was already forwarded on `event_tx`.
///
/// Carries the text accumulated before the failure so the loop can persist
/// the visible partial. Unlike a pre-output stream failure, this must NOT
/// trigger the non-streaming fallback: a retry would duplicate already
/// delivered output on append-only consumers (WS/RPC/ACP).
#[derive(Debug)]
pub(crate) struct StreamInterruptedAfterOutput {
    pub(crate) partial_text: String,
    pub(crate) message: String,
}

impl std::fmt::Display for StreamInterruptedAfterOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StreamInterruptedAfterOutput {}

/// The user cancelled mid-stream *after* caller-visible text was already
/// forwarded on `event_tx`.
///
/// Carries the forwarded text so the loop can persist the visible partial
/// with the `[interrupted by user]` marker — the pre-consolidation streaming
/// engine committed the watched partial on cancel, and losing it makes the
/// transcript disagree with what the user saw stream. Chains to
/// [`ToolLoopCancelled`] via `source()`, so [`is_tool_loop_cancelled`] (and
/// every caller built on it: the no-fallback rule, the fixed observer
/// message, the wrappers' cancel arms) recognizes it unchanged.
#[derive(Debug)]
pub(crate) struct StreamCancelledAfterOutput {
    pub(crate) partial_text: String,
    cause: ToolLoopCancelled,
}

impl StreamCancelledAfterOutput {
    pub(crate) fn new(partial_text: String) -> Self {
        Self {
            partial_text,
            cause: ToolLoopCancelled,
        }
    }
}

impl std::fmt::Display for StreamCancelledAfterOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled after streamed output")
    }
}

impl std::error::Error for StreamCancelledAfterOutput {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

#[derive(Debug)]
pub struct ModelSwitchRequested {
    pub model_provider: String,
    pub model: String,
}

impl std::fmt::Display for ModelSwitchRequested {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "model switch requested to {} {}",
            self.model_provider, self.model
        )
    }
}

impl std::error::Error for ModelSwitchRequested {}

pub fn is_model_switch_requested(err: &anyhow::Error) -> Option<(String, String)> {
    err.chain()
        .filter_map(|source| source.downcast_ref::<ModelSwitchRequested>())
        .map(|e| (e.model_provider.clone(), e.model.clone()))
        .next()
}
