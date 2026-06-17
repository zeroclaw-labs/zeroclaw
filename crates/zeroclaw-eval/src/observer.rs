//! An [`Observer`] that records tool-call outcomes and token usage from a run.
//!
//! This is the clean seam for trajectory/cost capture: the agent loop already
//! emits [`ObserverEvent`]s for every tool call and LLM response, so the eval
//! harness observes them without touching the agent. It is the Phase 0 seed of
//! the richer run-record capture used by later phases.

use std::sync::Mutex;
use zeroclaw_api::observability_traits::{Observer, ObserverEvent, ObserverMetric};

/// Captures `(tool_name, success)` for each dispatched tool call and accumulates
/// reported token usage across the run.
#[derive(Default)]
pub struct RecordingObserver {
    tool_calls: Mutex<Vec<(String, bool)>>,
    input_tokens: Mutex<u64>,
    output_tokens: Mutex<u64>,
}

impl RecordingObserver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Names of tools that were dispatched, in call order.
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_calls
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// True when every dispatched tool call reported success (vacuously true if none).
    pub fn all_tools_succeeded(&self) -> bool {
        self.tool_calls.lock().unwrap().iter().all(|(_, ok)| *ok)
    }

    /// Accumulated `(input_tokens, output_tokens)` reported by the provider.
    pub fn tokens(&self) -> (u64, u64) {
        (
            *self.input_tokens.lock().unwrap(),
            *self.output_tokens.lock().unwrap(),
        )
    }
}

impl Observer for RecordingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            ObserverEvent::ToolCall { tool, success, .. } => {
                self.tool_calls
                    .lock()
                    .unwrap()
                    .push((tool.clone(), *success));
            }
            ObserverEvent::LlmResponse {
                input_tokens,
                output_tokens,
                ..
            } => {
                if let Some(i) = input_tokens {
                    *self.input_tokens.lock().unwrap() += i;
                }
                if let Some(o) = output_tokens {
                    *self.output_tokens.lock().unwrap() += o;
                }
            }
            _ => {}
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn name(&self) -> &str {
        "eval-recording"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
