//! Observer bridge — projects [`crate::LogEvent`]s onto the typed
//! [`zeroclaw_api::observability_traits::ObserverEvent`] variants when a
//! bound observer is installed.
//!
//! Lets metrics backends (Prometheus, OTel) consume the same single
//! emission stream as the JSONL log and the SSE broadcast. The
//! projection is bounded: only the actions that map to a known variant
//! get forwarded, and only the metric-relevant subset of fields
//! crosses the boundary (the high-cardinality content like message body
//! and attributes does not).
//!
//! Install via [`set_observer_bridge`]; bridge is invoked once per event
//! by `writer::record_event`. Missing observer = no-op; unmapped action
//! = no-op.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::RwLock;
use zeroclaw_api::observability_traits::{Observer, ObserverEvent};

use crate::event::LogEvent;

static OBSERVER: OnceLock<RwLock<Option<Arc<dyn Observer>>>> = OnceLock::new();

fn slot() -> &'static RwLock<Option<Arc<dyn Observer>>> {
    OBSERVER.get_or_init(|| RwLock::new(None))
}

/// Install the bound Observer that the bridge forwards events to.
/// Calling again replaces the previous binding.
pub fn set_observer_bridge(observer: Arc<dyn Observer>) {
    *slot().write() = Some(observer);
}

/// Remove the Observer binding (tests, orderly shutdown).
pub fn clear_observer_bridge() {
    *slot().write() = None;
}

/// Project a [`LogEvent`] onto an [`ObserverEvent`] variant when the
/// action is one the typed surface understands, and forward to the
/// bound observer. No-op when no observer is bound or the action does
/// not map.
pub(crate) fn forward(event: &LogEvent) {
    let Some(observer) = slot().read().clone() else {
        return;
    };
    if let Some(obs_event) = project(event) {
        observer.record_event(&obs_event);
    }
}

fn project(event: &LogEvent) -> Option<ObserverEvent> {
    use crate::event::type_field;
    let action = event.event.action.as_str();
    let attribution = &event.zeroclaw;
    let model_provider = attribution
        .get(&type_field("model_provider"))
        .or_else(|| attribution.get("model_provider"))
        .unwrap_or_default()
        .to_string();
    let model = attribution.get("model").unwrap_or_default().to_string();
    let tool = attribution.get("tool").unwrap_or_default().to_string();
    let channel = attribution.get("channel").unwrap_or_default().to_string();
    let duration = attribution
        .duration_ms
        .map(Duration::from_millis)
        .unwrap_or_default();
    let success = matches!(event.event.outcome.as_str(), "success");

    match action {
        "agent_start" => Some(ObserverEvent::AgentStart {
            model_provider,
            model,
        }),
        "agent_end" => Some(ObserverEvent::AgentEnd {
            model_provider,
            model,
            duration,
            tokens_used: event
                .attributes
                .get("tokens_used")
                .and_then(serde_json::Value::as_u64),
            cost_usd: event
                .attributes
                .get("cost_usd")
                .and_then(serde_json::Value::as_f64),
        }),
        "llm_request" => Some(ObserverEvent::LlmRequest {
            model_provider,
            model,
            messages_count: event
                .attributes
                .get("messages_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default() as usize,
        }),
        "llm_response" => Some(ObserverEvent::LlmResponse {
            model_provider,
            model,
            duration,
            success,
            error_message: event
                .attributes
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            input_tokens: event
                .attributes
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64),
            output_tokens: event
                .attributes
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64),
            messages: None,
        }),
        "tool_call_start" => Some(ObserverEvent::ToolCallStart {
            tool,
            tool_call_id: None,
            arguments: None,
        }),
        "tool_call" | "tool_call_result" => Some(ObserverEvent::ToolCall {
            tool,
            tool_call_id: None,
            duration,
            success,
            arguments: None,
            result: None,
        }),
        "channel_message_inbound" => Some(ObserverEvent::ChannelMessage {
            channel,
            direction: "inbound".to_string(),
        }),
        "channel_send" => Some(ObserverEvent::ChannelMessage {
            channel,
            direction: "outbound".to_string(),
        }),
        "turn_complete" => Some(ObserverEvent::TurnComplete),
        "heartbeat_tick" => Some(ObserverEvent::HeartbeatTick),
        "error" => Some(ObserverEvent::Error {
            component: attribution
                .get(&type_field("channel"))
                .unwrap_or("system")
                .to_string(),
            message: event.message.clone().unwrap_or_default(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventCategory, EventOutcome, Severity};
    use std::any::Any;
    use std::sync::Mutex;
    use zeroclaw_api::observability_traits::ObserverMetric;

    #[derive(Default)]
    struct CapturingObserver {
        events: Mutex<Vec<ObserverEvent>>,
    }

    impl Observer for CapturingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
        fn record_metric(&self, _metric: &ObserverMetric) {}
        fn name(&self) -> &str {
            "capturing"
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    static BRIDGE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn projects_llm_request() {
        let _guard = BRIDGE_LOCK.lock();
        clear_observer_bridge();
        let observer = Arc::new(CapturingObserver::default());
        set_observer_bridge(observer.clone());

        let mut event = LogEvent::new(Severity::Info, "llm_request", EventCategory::Agent);
        event
            .zeroclaw
            .set_composite("model_provider", "anthropic.clamps");
        event.zeroclaw.set("model", "claude-sonnet-4-6");
        event.attributes = serde_json::json!({ "messages_count": 4 });

        forward(&event);

        let projected = observer.events.lock().unwrap();
        assert_eq!(projected.len(), 1);
        match &projected[0] {
            ObserverEvent::LlmRequest {
                model_provider,
                model,
                messages_count,
            } => {
                assert_eq!(model_provider, "anthropic");
                assert_eq!(model, "claude-sonnet-4-6");
                assert_eq!(*messages_count, 4);
            }
            other => panic!("expected LlmRequest, got {other:?}"),
        }

        clear_observer_bridge();
    }

    #[test]
    fn projects_tool_call_success() {
        let _guard = BRIDGE_LOCK.lock();
        clear_observer_bridge();
        let observer = Arc::new(CapturingObserver::default());
        set_observer_bridge(observer.clone());

        let mut event = LogEvent::new(Severity::Info, "tool_call", EventCategory::Tool);
        event.zeroclaw.set("tool", "shell");
        event.zeroclaw.duration_ms = Some(120);
        event.set_outcome(EventOutcome::Success);

        forward(&event);

        let projected = observer.events.lock().unwrap();
        assert_eq!(projected.len(), 1);
        match &projected[0] {
            ObserverEvent::ToolCall {
                tool,
                duration,
                success,
                ..
            } => {
                assert_eq!(tool, "shell");
                assert_eq!(*duration, Duration::from_millis(120));
                assert!(*success);
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        clear_observer_bridge();
    }

    #[test]
    fn unknown_action_is_noop() {
        let _guard = BRIDGE_LOCK.lock();
        clear_observer_bridge();
        let observer = Arc::new(CapturingObserver::default());
        set_observer_bridge(observer.clone());

        let event = LogEvent::new(Severity::Info, "totally_made_up", EventCategory::System);
        forward(&event);

        assert!(observer.events.lock().unwrap().is_empty());
        clear_observer_bridge();
    }
}
