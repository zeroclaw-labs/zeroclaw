//! Agent message channel trait and in-memory implementation.
//!
//! This module provides the `AgentMessageChannel` trait for inter-agent
//! communication and a memory-backed implementation for single-process
//! multi-agent coordination.

use async_trait::async_trait;
use crate::coordination::message::{AgentId, AgentMessage, MessagePayload};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

/// Error type for message channel operations.
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("timeout waiting for message")]
    Timeout,

    #[error("agent not found: {0}")]
    AgentNotFound(String),

    #[error("channel closed")]
    Closed,

    #[error("invalid message: {0}")]
    InvalidMessage(String),

    #[error("request {0} not found")]
    RequestNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for channel operations.
pub type ChannelResult<T> = Result<T, ChannelError>;

/// Channel for agent communication.
///
/// This trait defines the interface for sending and receiving messages
/// between agents. Implementations can use different backends (memory,
/// SQLite, Redis) while providing the same API.
#[async_trait]
pub trait AgentMessageChannel: Send + Sync {
    /// Send a message (returns immediately for non-blocking).
    async fn send(&self, message: AgentMessage) -> ChannelResult<()>;

    /// Receive a message for this agent (blocks until available or timeout).
    async fn receive(
        &self,
        agent_id: &AgentId,
        timeout: std::time::Duration,
    ) -> ChannelResult<AgentMessage>;

    /// Send request and wait for response.
    async fn request(
        &self,
        to: AgentId,
        payload: MessagePayload,
        timeout: std::time::Duration,
    ) -> ChannelResult<MessagePayload>;

    /// Check for pending messages without blocking.
    async fn peek(&self, agent_id: &AgentId) -> ChannelResult<usize>;

    /// Clear all messages for an agent.
    async fn clear(&self, agent_id: &AgentId) -> ChannelResult<()>;

    /// Register an agent to receive messages.
    async fn register(&self, agent_id: &AgentId) -> ChannelResult<()>;

    /// Unregister an agent from receiving messages.
    async fn unregister(&self, agent_id: &AgentId) -> ChannelResult<bool>;
}

/// Internal inbox state for a single agent.
#[derive(Debug, Default)]
struct AgentInbox {
    messages: VecDeque<AgentMessage>,
    response_waiters: HashMap<String, tokio::sync::oneshot::Sender<MessagePayload>>,
}

impl AgentInbox {
    fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            response_waiters: HashMap::new(),
        }
    }
}

/// In-memory message channel implementation.
///
/// This implementation stores messages in memory and provides
/// efficient intra-process communication between agents.
#[derive(Debug, Clone)]
pub struct MemoryMessageChannel {
    inner: Arc<Mutex<MemoryChannelState>>,
}

#[derive(Debug)]
struct MemoryChannelState {
    inboxes: HashMap<String, AgentInbox>,
    broadcast_tx: broadcast::Sender<AgentMessage>,
}

impl Default for MemoryMessageChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryMessageChannel {
    /// Create a new in-memory message channel.
    pub fn new() -> Self {
        let (broadcast_tx, _) = broadcast::channel(1000);
        Self {
            inner: Arc::new(Mutex::new(MemoryChannelState {
                inboxes: HashMap::new(),
                broadcast_tx,
            })),
        }
    }

    /// Create a new channel with a specific broadcast channel capacity.
    pub fn with_broadcast_capacity(capacity: usize) -> Self {
        let (broadcast_tx, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(Mutex::new(MemoryChannelState {
                inboxes: HashMap::new(),
                broadcast_tx,
            })),
        }
    }

    /// Get the number of registered agents.
    pub async fn agent_count(&self) -> usize {
        let state = self.inner.lock().await;
        state.inboxes.len()
    }

    /// Get a list of all registered agent IDs.
    pub async fn registered_agents(&self) -> Vec<String> {
        let state = self.inner.lock().await;
        state.inboxes.keys().cloned().collect()
    }

    /// Subscribe to broadcast messages.
    pub fn subscribe_broadcast(&self) -> broadcast::Receiver<AgentMessage> {
        let state = self.inner.try_lock();
        if let Ok(state) = state {
            state.broadcast_tx.subscribe()
        } else {
            // If lock is held, create a new receiver from the default
            broadcast::channel(1000).1
        }
    }

    /// Internal method to deliver a message to an agent's inbox.
    async fn deliver_to_inbox(&self, agent_id: &str, message: AgentMessage) -> ChannelResult<()> {
        let mut state = self.inner.lock().await;
        let inbox = state.inboxes.get_mut(agent_id)
            .ok_or_else(|| ChannelError::AgentNotFound(agent_id.to_string()))?;

        match &message {
            AgentMessage::Response { request_id, payload, .. } => {
                // Check if there's a waiter for this response
                if let Some(waiter) = inbox.response_waiters.remove(request_id) {
                    let _ = waiter.send(payload.clone());
                } else {
                    // No waiter, store in inbox
                    inbox.messages.push_back(message);
                }
            }
            _ => {
                inbox.messages.push_back(message);
            }
        }
        Ok(())
    }

    /// Internal method to deliver a broadcast message.
    async fn deliver_broadcast(&self, message: &AgentMessage) -> ChannelResult<usize> {
        let mut state = self.inner.lock().await;
        let from_id = message.from().as_str();

        let mut delivered = 0;
        for (agent_id, inbox) in state.inboxes.iter_mut() {
            // Don't deliver broadcasts to the sender
            if agent_id != from_id {
                inbox.messages.push_back(message.clone());
                delivered += 1;
            }
        }
        Ok(delivered)
    }
}

#[async_trait]
impl AgentMessageChannel for MemoryMessageChannel {
    async fn send(&self, message: AgentMessage) -> ChannelResult<()> {
        match &message {
            AgentMessage::Request { to, .. } | AgentMessage::Notification { to, .. } => {
                self.deliver_to_inbox(to.as_str(), message.clone()).await?;
            }
            AgentMessage::Broadcast { .. } => {
                self.deliver_broadcast(&message).await?;
                // Also send via broadcast channel for subscribers
                let state = self.inner.lock().await;
                let _ = state.broadcast_tx.send(message);
            }
            AgentMessage::Response { request_id, from, .. } => {
                // Responses need special handling - try to find the original requester
                let mut state = self.inner.lock().await;
                let mut delivered = false;

                // Search all inboxes for a response waiter
                for inbox in state.inboxes.values_mut() {
                    if let Some(waiter) = inbox.response_waiters.remove(request_id) {
                        let payload = message.payload().clone();
                        let _ = waiter.send(payload);
                        delivered = true;
                        break;
                    }
                }

                if !delivered {
                    // No waiter found, deliver to sender's inbox
                    let agent_id = from.as_str();
                    if let Some(inbox) = state.inboxes.get_mut(agent_id) {
                        inbox.messages.push_back(message);
                    }
                }
            }
        }
        Ok(())
    }

    async fn receive(
        &self,
        agent_id: &AgentId,
        timeout: std::time::Duration,
    ) -> ChannelResult<AgentMessage> {
        let agent_id_str = agent_id.as_str().to_string();

        // Create a future that waits for a message
        let receive_future = async {
            loop {
                // Try to get a message from the inbox
                {
                    let mut state = self.inner.lock().await;
                    if let Some(inbox) = state.inboxes.get_mut(&agent_id_str) {
                        if let Some(message) = inbox.messages.pop_front() {
                            return Ok(message);
                        }
                    }
                }
                // No message available, wait a bit and retry
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        };

        // Apply timeout
        tokio::time::timeout(timeout, receive_future)
            .await
            .map_err(|_| ChannelError::Timeout)?
    }

    async fn request(
        &self,
        to: AgentId,
        payload: MessagePayload,
        timeout: std::time::Duration,
    ) -> ChannelResult<MessagePayload> {
        let from = AgentId::generate();
        let request_msg = AgentMessage::request(from.clone(), to.clone(), payload);
        let request_id = request_msg.id().to_string();

        // Create a response waiter
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut state = self.inner.lock().await;
            let from_id = from.as_str();
            let inbox = state.inboxes.get_mut(from_id)
                .ok_or_else(|| ChannelError::AgentNotFound(from_id.to_string()))?;
            inbox.response_waiters.insert(request_id.clone(), tx);
        }

        // Send the request
        self.send(request_msg).await?;

        // Wait for response
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| ChannelError::Timeout)?
            .map_err(|_| ChannelError::RequestNotFound(request_id))
    }

    async fn peek(&self, agent_id: &AgentId) -> ChannelResult<usize> {
        let state = self.inner.lock().await;
        let inbox = state.inboxes.get(agent_id.as_str())
            .ok_or_else(|| ChannelError::AgentNotFound(agent_id.to_string()))?;
        Ok(inbox.messages.len())
    }

    async fn clear(&self, agent_id: &AgentId) -> ChannelResult<()> {
        let mut state = self.inner.lock().await;
        let inbox = state.inboxes.get_mut(agent_id.as_str())
            .ok_or_else(|| ChannelError::AgentNotFound(agent_id.to_string()))?;
        inbox.messages.clear();
        Ok(())
    }

    async fn register(&self, agent_id: &AgentId) -> ChannelResult<()> {
        let mut state = self.inner.lock().await;
        let agent_id_str = agent_id.as_str().to_string();
        if state.inboxes.contains_key(&agent_id_str) {
            return Ok(());
        }
        state.inboxes.insert(agent_id_str, AgentInbox::new());
        Ok(())
    }

    async fn unregister(&self, agent_id: &AgentId) -> ChannelResult<bool> {
        let mut state = self.inner.lock().await;
        Ok(state.inboxes.remove(agent_id.as_str()).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::message::AgentState;
    use std::collections::HashMap;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn register_and_unregister_agent() {
        let channel = MemoryMessageChannel::new();
        let agent_id = AgentId::generate();

        channel.register(&agent_id).await.unwrap();
        assert_eq!(channel.agent_count().await, 1);

        let removed = channel.unregister(&agent_id).await.unwrap();
        assert!(removed);
        assert_eq!(channel.agent_count().await, 0);

        let removed_again = channel.unregister(&agent_id).await.unwrap();
        assert!(!removed_again);
    }

    #[tokio::test]
    async fn send_and_receive_notification() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();

        let payload = MessagePayload::text("hello from a");
        let msg = AgentMessage::notification(agent_a.clone(), agent_b.clone(), payload);

        channel.send(msg).await.unwrap();

        let received = channel
            .receive(&agent_b, Duration::from_millis(100))
            .await
            .unwrap();

        assert_eq!(received.from(), &agent_a);
        assert_eq!(received.to(), Some(&agent_b));
        assert_eq!(received.payload().as_text(), Some("hello from a"));
    }

    #[tokio::test]
    async fn send_and_receive_broadcast() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());
        let agent_c = AgentId::new("agent_c".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();
        channel.register(&agent_c).await.unwrap();

        let payload = MessagePayload::text("broadcast message");
        let msg = AgentMessage::broadcast(agent_a.clone(), payload);

        channel.send(msg).await.unwrap();

        // Agent B should receive the broadcast
        let received_b = channel
            .receive(&agent_b, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(received_b.from(), &agent_a);
        assert_eq!(received_b.payload().as_text(), Some("broadcast message"));

        // Agent C should also receive the broadcast
        let received_c = channel
            .receive(&agent_c, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(received_c.from(), &agent_a);
        assert_eq!(received_c.payload().as_text(), Some("broadcast message"));

        // Agent A should NOT receive their own broadcast
        assert!(channel
            .receive(&agent_a, Duration::from_millis(100))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn request_response_pattern() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();

        // Spawn a task to handle the request
        let channel_clone = channel.clone();
        let agent_b_clone = agent_b.clone();
        tokio::spawn(async move {
            let msg = channel_clone
                .receive(&agent_b_clone, Duration::from_secs(5))
                .await
                .unwrap();
            if let AgentMessage::Request { id, from: _, .. } = msg {
                let response = AgentMessage::response(
                    id.clone(),
                    agent_b_clone,
                    MessagePayload::text("response data"),
                );
                channel_clone.send(response).await.unwrap();
            }
        });

        // Send request from agent A
        let response_payload = channel
            .request(
                agent_b.clone(),
                MessagePayload::text("request data"),
                Duration::from_secs(5),
            )
            .await
            .unwrap();

        assert_eq!(response_payload.as_text(), Some("response data"));
    }

    #[tokio::test]
    async fn request_timeout() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();

        // No one to respond, so timeout
        let result = channel
            .request(
                agent_b.clone(),
                MessagePayload::text("request"),
                Duration::from_millis(100),
            )
            .await;

        assert!(matches!(result, Err(ChannelError::Timeout)));
    }

    #[tokio::test]
    async fn peek_pending_messages() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();

        // Send multiple messages
        for i in 0..3 {
            let msg = AgentMessage::notification(
                agent_a.clone(),
                agent_b.clone(),
                MessagePayload::text(format!("message {}", i)),
            );
            channel.send(msg).await.unwrap();
        }

        let count = channel.peek(&agent_b).await.unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn clear_messages() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();

        // Send a message
        let msg = AgentMessage::notification(
            agent_a.clone(),
            agent_b.clone(),
            MessagePayload::text("test"),
        );
        channel.send(msg).await.unwrap();

        assert_eq!(channel.peek(&agent_b).await.unwrap(), 1);

        // Clear messages
        channel.clear(&agent_b).await.unwrap();
        assert_eq!(channel.peek(&agent_b).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn receive_from_nonexistent_agent_returns_error() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());

        let result = channel
            .receive(&agent_a, Duration::from_millis(100))
            .await;

        assert!(matches!(result, Err(ChannelError::Timeout)));
    }

    #[tokio::test]
    async fn send_to_nonexistent_agent_returns_error() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        channel.register(&agent_a).await.unwrap();
        // Don't register agent_b

        let msg = AgentMessage::notification(
            agent_a.clone(),
            agent_b.clone(),
            MessagePayload::text("test"),
        );

        let result = channel.send(msg).await;
        assert!(matches!(result, Err(ChannelError::AgentNotFound(_))));
    }

    #[tokio::test]
    async fn message_ordering_is_preserved() {
        let channel = MemoryMessageChannel::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();

        // Send messages in order
        for i in 0..5 {
            let msg = AgentMessage::notification(
                agent_a.clone(),
                agent_b.clone(),
                MessagePayload::text(format!("message {}", i)),
            );
            channel.send(msg).await.unwrap();
        }

        // Receive and verify order
        for i in 0..5 {
            let msg = channel
                .receive(&agent_b, Duration::from_millis(100))
                .await
                .unwrap();
            assert_eq!(msg.payload().as_text(), Some(format!("message {}", i).as_str()));
        }
    }
}
