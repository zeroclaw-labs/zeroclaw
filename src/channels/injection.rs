//! Mid-turn message injection for concurrent inbound messages.
//!
//! When a user sends a new message while the agent loop is already running
//! for that sender, the message is pushed onto a per-conversation injection
//! queue instead of waiting for the current turn to finish. The agent loop
//! drains the queue after each tool-execution batch, injecting the messages
//! as `ChatMessage::user(...)` entries so the LLM sees them on the next
//! iteration.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A single message injected mid-turn from a concurrent inbound message.
#[derive(Debug, Clone)]
pub struct InjectedMessage {
    /// The raw user content.
    pub content: String,
    /// The originating channel name, for tracing.
    pub channel: String,
    /// The originating sender id, for tracing.
    pub sender: String,
}

/// Sender half stored in `ChannelRuntimeContext`, keyed by conversation history key.
pub type InjectionSender = tokio::sync::mpsc::UnboundedSender<InjectedMessage>;

/// Receiver half owned exclusively by the running tool loop.
pub type InjectionReceiver = tokio::sync::mpsc::UnboundedReceiver<InjectedMessage>;

/// Shared map: conversation_history_key → active sender for that conversation's running loop.
pub type InjectionQueueMap = Arc<Mutex<HashMap<String, InjectionSender>>>;

/// RAII guard that removes the injection sender from the shared map on drop.
pub(crate) struct InjectionGuard {
    queues: InjectionQueueMap,
    key: String,
}

impl InjectionGuard {
    pub fn new(queues: InjectionQueueMap, key: String) -> Self {
        Self { queues, key }
    }
}

impl Drop for InjectionGuard {
    fn drop(&mut self) {
        let mut q = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        q.remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_guard_removes_entry_on_drop() {
        let map: InjectionQueueMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<InjectedMessage>();
        map.lock().unwrap().insert("test_key".to_string(), tx);

        assert!(map.lock().unwrap().contains_key("test_key"));
        {
            let _guard = InjectionGuard::new(Arc::clone(&map), "test_key".to_string());
        }
        assert!(!map.lock().unwrap().contains_key("test_key"));
    }

    #[test]
    fn injection_guard_handles_already_removed_key() {
        let map: InjectionQueueMap = Arc::new(Mutex::new(HashMap::new()));
        // Don't insert anything — guard should not panic on drop
        let _guard = InjectionGuard::new(map, "nonexistent".to_string());
    }
}
