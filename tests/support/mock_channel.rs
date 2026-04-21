//! Mock channel for system-level tests.
//!
//! `TestChannel` implements the `Channel` trait with MPSC-based message
//! injection and response capture for race-free testing.

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use zeroclaw::channels::{Channel, ChannelMessage, SendMessage};

/// A test channel that captures sent messages and supports message injection.
pub struct TestChannel {
    name: String,
    sent_messages: Arc<Mutex<Vec<SendMessage>>>,
    typing_events: Arc<Mutex<Vec<TypingEvent>>>,
}

#[derive(Debug, Clone)]
pub enum TypingEvent {
    Start(String),
    Stop(String),
}

impl TestChannel {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            sent_messages: Arc::new(Mutex::new(Vec::new())),
            typing_events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get all messages sent through this channel.
    pub fn sent_messages(&self) -> Vec<SendMessage> {
        self.sent_messages.lock().unwrap().clone()
    }

    /// Get all typing events recorded by this channel.
    pub fn typing_events(&self) -> Vec<TypingEvent> {
        self.typing_events.lock().unwrap().clone()
    }

    /// Clear captured messages and events.
    pub fn clear(&self) {
        self.sent_messages.lock().unwrap().clear();
        self.typing_events.lock().unwrap().clear();
    }
}

#[async_trait]
impl Channel for TestChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.sent_messages.lock().unwrap().push(message.clone());
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // System tests drive the agent via turn() rather than channel listen,
        // so this is a no-op. For channel-driven tests, messages are injected
        // via the MPSC sender directly.
        Ok(())
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.typing_events
            .lock()
            .unwrap()
            .push(TypingEvent::Start(recipient.to_string()));
        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.typing_events
            .lock()
            .unwrap()
            .push(TypingEvent::Stop(recipient.to_string()));
        Ok(())
    }
}
