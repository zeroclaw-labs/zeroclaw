//! An [`Observer`] that records tool-call outcomes and token usage from a run.

use std::sync::{Mutex, MutexGuard};
use zeroclaw_api::observability_traits::{Observer, ObserverEvent, ObserverMetric};

/// Captures `(tool_name, success)` for each dispatched tool call and accumulates
/// reported token usage across the run.
#[derive(Default)]
pub struct RecordingObserver {
    tool_calls: Mutex<Vec<(String, bool)>>,
    input_tokens: Mutex<u64>,
    output_tokens: Mutex<u64>,
    llm_calls: Mutex<u32>,
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl RecordingObserver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Names of tools that were dispatched, in call order.
    pub fn tool_names(&self) -> Vec<String> {
        lock_recover(&self.tool_calls)
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// True when every dispatched tool call reported success (vacuously true if none).
    pub fn all_tools_succeeded(&self) -> bool {
        lock_recover(&self.tool_calls).iter().all(|(_, ok)| *ok)
    }

    /// Accumulated `(input_tokens, output_tokens)` reported by the provider.
    pub fn tokens(&self) -> (u64, u64) {
        (
            *lock_recover(&self.input_tokens),
            *lock_recover(&self.output_tokens),
        )
    }

    /// Number of LLM responses observed during the run.
    pub fn llm_calls(&self) -> u32 {
        *lock_recover(&self.llm_calls)
    }
}

impl Observer for RecordingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            ObserverEvent::ToolCall { tool, success, .. } => {
                lock_recover(&self.tool_calls).push((tool.clone(), *success));
            }
            ObserverEvent::LlmResponse {
                input_tokens,
                output_tokens,
                ..
            } => {
                let mut llm_calls = lock_recover(&self.llm_calls);
                *llm_calls = llm_calls.saturating_add(1);
                if let Some(i) = input_tokens {
                    let mut total = lock_recover(&self.input_tokens);
                    *total = total.saturating_add(*i);
                }
                if let Some(o) = output_tokens {
                    let mut total = lock_recover(&self.output_tokens);
                    *total = total.saturating_add(*o);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use zeroclaw_api::observability_traits::{Observer, ObserverEvent};

    fn tool_call_event(tool: &str, success: bool) -> ObserverEvent {
        ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: tool.to_string(),
            tool_call_id: None,
            duration: Duration::from_millis(10),
            success,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        }
    }

    fn llm_event(input: u64, output: u64) -> ObserverEvent {
        ObserverEvent::LlmResponse {
            parent_agent_alias: None,
            model_provider: String::new(),
            model: String::new(),
            duration: Duration::from_millis(50),
            success: true,
            error_message: None,
            input_tokens: Some(input),
            output_tokens: Some(output),
            messages: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        }
    }

    #[test]
    fn new_observer_is_empty() {
        let obs = RecordingObserver::new();
        assert!(obs.tool_names().is_empty());
        assert!(obs.all_tools_succeeded());
        assert_eq!(obs.tokens(), (0, 0));
    }

    #[test]
    fn tool_names_records_order() {
        let obs = RecordingObserver::new();
        obs.record_event(&tool_call_event("search", true));
        obs.record_event(&tool_call_event("write", true));
        assert_eq!(
            obs.tool_names(),
            vec!["search".to_string(), "write".to_string()]
        );
    }

    #[test]
    fn all_tools_succeeded_false_on_failure() {
        let obs = RecordingObserver::new();
        obs.record_event(&tool_call_event("ok", true));
        obs.record_event(&tool_call_event("bad", false));
        assert!(!obs.all_tools_succeeded());
    }

    #[test]
    fn tokens_accumulate_across_llm_responses() {
        let obs = RecordingObserver::new();
        obs.record_event(&llm_event(100, 50));
        obs.record_event(&llm_event(200, 80));
        assert_eq!(obs.tokens(), (300, 130));
    }

    #[test]
    fn token_totals_saturate_on_provider_overflow() {
        let obs = RecordingObserver::new();
        obs.record_event(&llm_event(u64::MAX, u64::MAX));
        obs.record_event(&llm_event(1, 1));
        assert_eq!(obs.tokens(), (u64::MAX, u64::MAX));
    }

    #[test]
    fn llm_calls_counted_per_response_event() {
        let obs = RecordingObserver::new();
        assert_eq!(obs.llm_calls(), 0);
        obs.record_event(&llm_event(1, 1));
        obs.record_event(&llm_event(1, 1));
        obs.record_event(&llm_event(1, 1));
        assert_eq!(obs.llm_calls(), 3);
        // Non-LLM events do not affect the count.
        obs.record_event(&tool_call_event("echo", true));
        assert_eq!(obs.llm_calls(), 3);
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let obs = RecordingObserver::new();
        obs.record_event(&ObserverEvent::HeartbeatTick);
        assert!(obs.tool_names().is_empty());
        assert_eq!(obs.tokens(), (0, 0));
    }
}
