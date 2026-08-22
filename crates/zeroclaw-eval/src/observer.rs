//! An [`Observer`] that records tool-call outcomes and token usage from a run.

use std::sync::Mutex;
use zeroclaw_api::observability_traits::{Observer, ObserverEvent, ObserverMetric};

/// One dispatched tool call as observed at the dispatch boundary.
///
/// Unlike the bare `(name, success)` pair this replaces, a `RecordedCall` carries the
/// arguments the agent actually dispatched and the output the tool actually returned.
/// That is what lets a fixture grade a round trip instead of grading text the replay
/// provider scripted for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    /// Tool name, as dispatched.
    pub name: String,
    /// Serialized JSON arguments the agent passed to the tool. Empty when the
    /// dispatch boundary did not report any.
    pub arguments: String,
    /// Tool output (or error reason) as returned. Empty when not reported.
    pub result: String,
    /// Whether the call reported success.
    pub success: bool,
}

/// Captures each dispatched tool call (name, arguments, result, success) and accumulates
/// reported token usage across the run.
#[derive(Default)]
pub struct RecordingObserver {
    tool_calls: Mutex<Vec<RecordedCall>>,
    input_tokens: Mutex<u64>,
    output_tokens: Mutex<u64>,
}

impl RecordingObserver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every dispatched tool call, in call order, with arguments and results.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.tool_calls.lock().unwrap().clone()
    }

    /// Names of tools that were dispatched, in call order.
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    /// True when every dispatched tool call reported success (vacuously true if none).
    pub fn all_tools_succeeded(&self) -> bool {
        self.tool_calls.lock().unwrap().iter().all(|c| c.success)
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
            ObserverEvent::ToolCall {
                tool,
                success,
                arguments,
                result,
                ..
            } => {
                self.tool_calls.lock().unwrap().push(RecordedCall {
                    name: tool.clone(),
                    arguments: arguments.clone().unwrap_or_default(),
                    result: result.clone().unwrap_or_default(),
                    success: *success,
                });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use zeroclaw_api::observability_traits::{Observer, ObserverEvent};

    fn tool_call_event(tool: &str, success: bool) -> ObserverEvent {
        tool_call_event_with(tool, success, None, None)
    }

    fn tool_call_event_with(
        tool: &str,
        success: bool,
        arguments: Option<&str>,
        result: Option<&str>,
    ) -> ObserverEvent {
        ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: tool.to_string(),
            tool_call_id: None,
            duration: Duration::from_millis(10),
            success,
            arguments: arguments.map(str::to_string),
            result: result.map(str::to_string),
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
    fn unrelated_events_are_ignored() {
        let obs = RecordingObserver::new();
        obs.record_event(&ObserverEvent::HeartbeatTick);
        assert!(obs.tool_names().is_empty());
        assert_eq!(obs.tokens(), (0, 0));
    }

    #[test]
    fn recording_observer_captures_tool_arguments_and_results() {
        // B1: a Unicode argument must survive the dispatch boundary byte-for-byte, and
        // the tool's own output must be recorded separately from any scripted response.
        let obs = RecordingObserver::new();
        obs.record_event(&tool_call_event_with(
            "echo",
            true,
            Some(r#"{"message":"naïve café 日本語 ✓"}"#),
            Some("naïve café 日本語 ✓"),
        ));

        let calls = obs.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "echo");
        assert_eq!(calls[0].arguments, r#"{"message":"naïve café 日本語 ✓"}"#);
        assert_eq!(calls[0].result, "naïve café 日本語 ✓");
        assert!(calls[0].success);
    }

    #[test]
    fn recorded_calls_default_to_empty_strings_when_boundary_omits_payload() {
        let obs = RecordingObserver::new();
        obs.record_event(&tool_call_event("echo", true));
        let calls = obs.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].arguments.is_empty());
        assert!(calls[0].result.is_empty());
    }

    #[test]
    fn calls_preserve_dispatch_order_with_payloads() {
        let obs = RecordingObserver::new();
        obs.record_event(&tool_call_event_with(
            "echo",
            true,
            Some(r#"{"message":"alpha"}"#),
            Some("alpha"),
        ));
        obs.record_event(&tool_call_event_with(
            "echo",
            true,
            Some(r#"{"message":"beta"}"#),
            Some("beta"),
        ));
        let calls = obs.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].arguments.contains("alpha"));
        assert!(calls[1].arguments.contains("beta"));
    }
}
