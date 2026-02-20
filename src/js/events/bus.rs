// Event bus for JS plugin hook system
//
// This module provides the EventBus for distributing events to subscribers.
// Uses tokio's broadcast channel for fan-out event delivery.
//
// # Design Notes
//
// - Fire-and-forget semantics: if no subscribers, emit succeeds but does nothing
// - Channel capacity of 1024 events prevents unbounded memory growth
// - Each subscriber gets their own receiver with independent cursor
// - Slow subscribers miss events (Lagged error) rather than blocking producers

use super::Event;
use tokio::sync::broadcast;

pub type EventSender = broadcast::Sender<Event>;
pub type EventReceiver = broadcast::Receiver<Event>;

#[derive(Clone)]
pub struct EventBus {
    sender: EventSender,
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self { sender }
    }

    pub fn emit(&self, event: Event) {
        // Fire and forget - if no subscribers, that's OK
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> EventReceiver {
        self.sender.subscribe()
    }

    pub fn sender(&self) -> &EventSender {
        &self.sender
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_subscribe() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        let event = Event::BeforeAgentStart {
            config: serde_json::json!({}),
        };

        bus.emit(event.clone());

        let received = rx.blocking_recv().unwrap();
        assert_eq!(received.name().as_ref(), "before.agent.start");
    }

    #[test]
    fn emit_without_subscribers() {
        let bus = EventBus::new();

        // Should not panic even with no subscribers
        let event = Event::BeforeAgentStart {
            config: serde_json::json!({}),
        };
        bus.emit(event);
    }

    #[test]
    fn multiple_subscribers() {
        let bus = EventBus::new();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        let event = Event::MessageReceived {
            channel_id: "test_channel".to_string(),
            channel_type: "test".to_string(),
            message: serde_json::json!({"content": "hello"}),
            session_id: None,
        };

        bus.emit(event.clone());

        // Both subscribers should receive the event
        let received1 = rx1.blocking_recv().unwrap();
        let received2 = rx2.blocking_recv().unwrap();

        assert_eq!(received1.name().as_ref(), "message.received");
        assert_eq!(received2.name().as_ref(), "message.received");
    }
}
