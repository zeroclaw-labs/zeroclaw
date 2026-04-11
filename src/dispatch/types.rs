//! Generic event types for the dispatch subsystem.
//!
//! These types are deliberately decoupled from any specific subsystem (SOP,
//! agent loop, channels, peripherals). Any subsystem that needs to publish
//! or react to ambient events uses `DispatchEvent` and registers an
//! `EventHandler` with the `EventRouter`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where the dispatch event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// MQTT message arrival.
    Mqtt,
    /// HTTP webhook delivery.
    Webhook,
    /// Cron schedule firing.
    Cron,
    /// Hardware peripheral signal (GPIO, sensor, etc.).
    Peripheral,
    /// Manually triggered (CLI, LLM tool, test).
    Manual,
}

impl std::fmt::Display for EventSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mqtt => write!(f, "mqtt"),
            Self::Webhook => write!(f, "webhook"),
            Self::Cron => write!(f, "cron"),
            Self::Peripheral => write!(f, "peripheral"),
            Self::Manual => write!(f, "manual"),
        }
    }
}

/// A dispatch event — an opaque payload routed to all matching handlers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchEvent {
    /// Unique event id (UUID v4).
    pub id: String,
    /// Where the event came from.
    pub source: EventSource,
    /// Optional logical topic (e.g., `nucleo-f401re/pin_3`, `/sop/deploy`).
    pub topic: Option<String>,
    /// Optional payload string (typically JSON, but free-form).
    pub payload: Option<String>,
    /// ISO-8601 UTC timestamp at the moment of construction.
    pub timestamp: String,
}

impl DispatchEvent {
    /// Construct a new event with a generated id and current timestamp.
    pub fn new(source: EventSource, topic: Option<String>, payload: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            source,
            topic,
            payload,
            timestamp: now_iso8601(),
        }
    }
}

/// Outcome reported by a single handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandlerOutcome {
    /// Handler successfully processed the event.
    Handled { summary: String },
    /// Handler matched on `matches()` but elected not to act.
    Skipped { reason: String },
    /// Handler returned an error or panicked.
    Failed { error: String },
}

/// Aggregate result of dispatching one event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchResult {
    /// The id of the dispatched event.
    pub event_id: String,
    /// Names of handlers that matched (in registration order).
    pub matched_handlers: Vec<String>,
    /// Per-handler outcomes, paired with their name.
    pub handler_outcomes: Vec<(String, HandlerOutcome)>,
}

impl DispatchResult {
    /// Convenience: how many handlers reported a successful `Handled` outcome.
    pub fn handled_count(&self) -> usize {
        self.handler_outcomes
            .iter()
            .filter(|(_, o)| matches!(o, HandlerOutcome::Handled { .. }))
            .count()
    }

    /// Convenience: how many handlers reported a `Failed` outcome.
    pub fn failed_count(&self) -> usize {
        self.handler_outcomes
            .iter()
            .filter(|(_, o)| matches!(o, HandlerOutcome::Failed { .. }))
            .count()
    }
}

/// Current UTC time in ISO-8601 / RFC-3339 format.
pub(crate) fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_source_display() {
        assert_eq!(EventSource::Mqtt.to_string(), "mqtt");
        assert_eq!(EventSource::Peripheral.to_string(), "peripheral");
    }

    #[test]
    fn dispatch_event_new_assigns_id_and_timestamp() {
        let e = DispatchEvent::new(
            EventSource::Manual,
            Some("test/topic".into()),
            Some("hello".into()),
        );
        assert_eq!(e.id.len(), 36); // UUID v4
        assert!(!e.timestamp.is_empty());
        assert_eq!(e.source, EventSource::Manual);
    }

    #[test]
    fn dispatch_result_counts() {
        let r = DispatchResult {
            event_id: "test".into(),
            matched_handlers: vec!["a".into(), "b".into(), "c".into()],
            handler_outcomes: vec![
                (
                    "a".into(),
                    HandlerOutcome::Handled {
                        summary: "ok".into(),
                    },
                ),
                (
                    "b".into(),
                    HandlerOutcome::Failed {
                        error: "boom".into(),
                    },
                ),
                (
                    "c".into(),
                    HandlerOutcome::Handled {
                        summary: "ok".into(),
                    },
                ),
            ],
        };
        assert_eq!(r.handled_count(), 2);
        assert_eq!(r.failed_count(), 1);
    }
}
