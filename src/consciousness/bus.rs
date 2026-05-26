use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::traits::{AgentKind, Priority};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub from: AgentKind,
    pub to: Option<AgentKind>,
    pub topic: String,
    pub payload: serde_json::Value,
    pub priority: Priority,
    pub timestamp: DateTime<Utc>,
}

impl BusMessage {
    pub fn new(
        from: AgentKind,
        to: Option<AgentKind>,
        topic: impl Into<String>,
        payload: serde_json::Value,
        priority: Priority,
    ) -> Self {
        Self {
            from,
            to,
            topic: topic.into(),
            payload,
            priority,
            timestamp: Utc::now(),
        }
    }

    pub fn broadcast(
        from: AgentKind,
        topic: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self::new(from, None, topic, payload, Priority::Normal)
    }
}

#[derive(Debug)]
pub struct SharedBus {
    messages: VecDeque<BusMessage>,
    capacity: usize,
}

impl SharedBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            messages: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn send(&mut self, message: BusMessage) {
        if self.messages.len() >= self.capacity {
            self.messages.pop_front();
        }
        self.messages.push_back(message);
    }

    pub fn drain_for(&mut self, target: AgentKind) -> Vec<BusMessage> {
        let mut targeted = Vec::new();
        let mut remaining = VecDeque::with_capacity(self.messages.len());

        for msg in self.messages.drain(..) {
            if msg.to.is_none() || msg.to == Some(target) {
                targeted.push(msg);
            } else {
                remaining.push_back(msg);
            }
        }

        self.messages = remaining;
        targeted
    }

    pub fn drain_all(&mut self) -> Vec<BusMessage> {
        self.messages.drain(..).collect()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_and_drain_targeted() {
        let mut bus = SharedBus::new(10);
        bus.send(BusMessage::new(
            AgentKind::Chairman,
            Some(AgentKind::Memory),
            "recall",
            serde_json::json!({"query": "test"}),
            Priority::Normal,
        ));
        bus.send(BusMessage::new(
            AgentKind::Chairman,
            Some(AgentKind::Strategy),
            "plan",
            serde_json::json!({}),
            Priority::Normal,
        ));

        let memory_msgs = bus.drain_for(AgentKind::Memory);
        assert_eq!(memory_msgs.len(), 1);
        assert_eq!(memory_msgs[0].topic, "recall");
        assert_eq!(bus.len(), 1);
    }

    #[test]
    fn broadcast_reaches_all() {
        let mut bus = SharedBus::new(10);
        bus.send(BusMessage::broadcast(
            AgentKind::Reflection,
            "coherence_update",
            serde_json::json!({"phi": 0.85}),
        ));

        let msgs = bus.drain_for(AgentKind::Chairman);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn capacity_overflow_drops_oldest() {
        let mut bus = SharedBus::new(2);
        bus.send(BusMessage::broadcast(
            AgentKind::Chairman,
            "first",
            serde_json::json!({}),
        ));
        bus.send(BusMessage::broadcast(
            AgentKind::Chairman,
            "second",
            serde_json::json!({}),
        ));
        bus.send(BusMessage::broadcast(
            AgentKind::Chairman,
            "third",
            serde_json::json!({}),
        ));

        assert_eq!(bus.len(), 2);
        let msgs = bus.drain_all();
        assert_eq!(msgs[0].topic, "second");
        assert_eq!(msgs[1].topic, "third");
    }
}
