use super::traits::{Observer, ObserverEvent, ObserverMetric};
use serde_json::json;
use std::any::Any;
use std::sync::{OnceLock, RwLock};

type EventSender = tokio::sync::broadcast::Sender<serde_json::Value>;

fn runtime_event_sender_slot() -> &'static RwLock<Option<EventSender>> {
    static SLOT: OnceLock<RwLock<Option<EventSender>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Register the shared runtime event stream sender used by frontend SSE.
///
/// Gateway startup should call this once so other components (channels, agent,
/// heartbeat) can publish events to the same `/api/events` stream.
pub fn set_runtime_event_sender(sender: EventSender) {
    let mut guard = runtime_event_sender_slot()
        .write()
        .unwrap_or_else(|e| e.into_inner());
    *guard = Some(sender);
}

fn runtime_event_sender() -> Option<EventSender> {
    runtime_event_sender_slot()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn map_event_to_json(event: &ObserverEvent) -> Option<serde_json::Value> {
    let value = match event {
        ObserverEvent::LlmRequest {
            provider, model, ..
        } => json!({
            "type": "llm_request",
            "provider": provider,
            "model": model,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        ObserverEvent::ToolCall {
            tool,
            duration,
            success,
        } => json!({
            "type": "tool_call",
            "tool": tool,
            "duration_ms": duration.as_millis(),
            "success": success,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        ObserverEvent::ToolCallStart { tool } => json!({
            "type": "tool_call_start",
            "tool": tool,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        ObserverEvent::Error { component, message } => json!({
            "type": "error",
            "component": component,
            "message": message,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        ObserverEvent::AgentStart { provider, model } => json!({
            "type": "agent_start",
            "provider": provider,
            "model": model,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        ObserverEvent::AgentEnd {
            provider,
            model,
            duration,
            tokens_used,
            cost_usd,
        } => json!({
            "type": "agent_end",
            "provider": provider,
            "model": model,
            "duration_ms": duration.as_millis(),
            "tokens_used": tokens_used,
            "cost_usd": cost_usd,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        ObserverEvent::ChannelMessage { channel, direction } => json!({
            "type": "channel_message",
            "channel": channel,
            "direction": direction,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        ObserverEvent::HeartbeatTick => json!({
            "type": "heartbeat",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        _ => return None,
    };

    Some(value)
}

/// Observer wrapper that forwards events to the shared runtime event stream.
///
/// It preserves the configured observer backend and augments it with
/// best-effort frontend log broadcasting.
pub struct RuntimeEventForwardingObserver {
    inner: Box<dyn Observer>,
}

impl RuntimeEventForwardingObserver {
    pub fn new(inner: Box<dyn Observer>) -> Self {
        Self { inner }
    }
}

impl Observer for RuntimeEventForwardingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        self.inner.record_event(event);

        let Some(payload) = map_event_to_json(event) else {
            return;
        };
        if let Some(sender) = runtime_event_sender() {
            let _ = sender.send(payload);
        }
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        self.inner.record_metric(metric);
    }

    fn flush(&self) {
        self.inner.flush();
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
