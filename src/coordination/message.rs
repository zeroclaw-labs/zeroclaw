//! Phase 1 agent message protocol types.
//!
//! This module defines the core types for inter-agent communication,
//! including message types, payloads, and agent identifiers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{self, Display};
use uuid::Uuid;

/// Message types for inter-agent communication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    /// Request-response pattern (blocking).
    Request {
        id: String,
        from: AgentId,
        to: AgentId,
        payload: MessagePayload,
        timestamp: chrono::DateTime<chrono::Utc>,
    },
    /// One-way notification (non-blocking).
    Notification {
        id: String,
        from: AgentId,
        to: AgentId,
        payload: MessagePayload,
        timestamp: chrono::DateTime<chrono::Utc>,
    },
    /// Broadcast to all agents.
    Broadcast {
        id: String,
        from: AgentId,
        payload: MessagePayload,
        timestamp: chrono::DateTime<chrono::Utc>,
    },
    /// Response to a request.
    Response {
        request_id: String,
        from: AgentId,
        payload: MessagePayload,
        timestamp: chrono::DateTime<chrono::Utc>,
    },
}

impl AgentMessage {
    /// Create a new request message.
    pub fn request(from: AgentId, to: AgentId, payload: MessagePayload) -> Self {
        Self::Request {
            id: Uuid::new_v4().to_string(),
            from,
            to,
            payload,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Create a new notification message.
    pub fn notification(from: AgentId, to: AgentId, payload: MessagePayload) -> Self {
        Self::Notification {
            id: Uuid::new_v4().to_string(),
            from,
            to,
            payload,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Create a new broadcast message.
    pub fn broadcast(from: AgentId, payload: MessagePayload) -> Self {
        Self::Broadcast {
            id: Uuid::new_v4().to_string(),
            from,
            payload,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Create a response message for a request.
    pub fn response(request_id: String, from: AgentId, payload: MessagePayload) -> Self {
        Self::Response {
            request_id,
            from,
            payload,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Get the message ID.
    pub fn id(&self) -> &str {
        match self {
            Self::Request { id, .. } | Self::Notification { id, .. } | Self::Broadcast { id, .. } => id,
            Self::Response { request_id, .. } => request_id,
        }
    }

    /// Get the sender agent ID.
    pub fn from(&self) -> &AgentId {
        match self {
            Self::Request { from, .. }
            | Self::Notification { from, .. }
            | Self::Broadcast { from, .. }
            | Self::Response { from, .. } => from,
        }
    }

    /// Get the target agent ID if applicable.
    pub fn to(&self) -> Option<&AgentId> {
        match self {
            Self::Request { to, .. } | Self::Notification { to, .. } => Some(to),
            Self::Broadcast { .. } | Self::Response { .. } => None,
        }
    }

    /// Get the message payload.
    pub fn payload(&self) -> &MessagePayload {
        match self {
            Self::Request { payload, .. }
            | Self::Notification { payload, .. }
            | Self::Broadcast { payload, .. }
            | Self::Response { payload, .. } => payload,
        }
    }

    /// Get the message timestamp.
    pub fn timestamp(&self) -> &chrono::DateTime<chrono::Utc> {
        match self {
            Self::Request { timestamp, .. }
            | Self::Notification { timestamp, .. }
            | Self::Broadcast { timestamp, .. }
            | Self::Response { timestamp, .. } => timestamp,
        }
    }
}

/// Message payload content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessagePayload {
    /// Simple text message.
    Text { content: String },
    /// Structured data (JSON-compatible).
    Data { value: serde_json::Value },
    /// Task delegation request.
    TaskDelegation {
        prompt: String,
        context: Option<String>,
        expected_format: Option<String>,
    },
    /// Status update.
    Status {
        state: AgentState,
        metadata: HashMap<String, String>,
    },
}

impl MessagePayload {
    /// Create a text message payload.
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text {
            content: content.into(),
        }
    }

    /// Create a data message payload.
    pub fn data(value: serde_json::Value) -> Self {
        Self::Data { value }
    }

    /// Create a task delegation payload.
    pub fn task_delegation(
        prompt: impl Into<String>,
        context: Option<String>,
        expected_format: Option<String>,
    ) -> Self {
        Self::TaskDelegation {
            prompt: prompt.into(),
            context,
            expected_format,
        }
    }

    /// Create a status update payload.
    pub fn status(state: AgentState, metadata: HashMap<String, String>) -> Self {
        Self::Status { state, metadata }
    }

    /// Extract text content if this is a text payload.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { content } => Some(content),
            _ => None,
        }
    }

    /// Extract data value if this is a data payload.
    pub fn as_data(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Data { value } => Some(value),
            _ => None,
        }
    }

    /// Convert to JSON representation.
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::Value::Null)
    }
}

impl From<String> for MessagePayload {
    fn from(content: String) -> Self {
        Self::Text { content }
    }
}

impl From<&str> for MessagePayload {
    fn from(content: &str) -> Self {
        Self::Text {
            content: content.to_string(),
        }
    }
}

impl From<serde_json::Value> for MessagePayload {
    fn from(value: serde_json::Value) -> Self {
        Self::Data { value }
    }
}

/// Agent state for status messages.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Agent is idle and available for work.
    Idle,
    /// Agent is actively processing.
    Working,
    /// Agent encountered an error.
    Error,
    /// Agent is shutting down.
    ShuttingDown,
}

impl AgentState {
    /// Check if the agent is available for new work.
    pub fn is_available(self) -> bool {
        matches!(self, Self::Idle)
    }
}

impl Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Working => write!(f, "working"),
            Self::Error => write!(f, "error"),
            Self::ShuttingDown => write!(f, "shutting_down"),
        }
    }
}

/// Unique agent identifier.
///
/// Agent IDs are used throughout the coordination system to address
/// messages and track agent state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(String);

impl AgentId {
    /// Create a new AgentId with the given string value.
    pub fn new(id: String) -> Self {
        Self(id)
    }

    /// Generate a unique AgentId.
    pub fn generate() -> Self {
        Self(format!("agent_{}", Uuid::new_v4()))
    }

    /// Create from delegate config name (backward compatibility).
    pub fn from_delegate_name(name: &str) -> Self {
        Self(format!("delegate:{}", name))
    }

    /// Get the underlying string value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Check if this is a delegate agent ID.
    pub fn is_delegate(&self) -> bool {
        self.0.starts_with("delegate:")
    }

    /// Get the delegate name if this is a delegate agent ID.
    pub fn delegate_name(&self) -> Option<&str> {
        self.0.strip_prefix("delegate:")
    }
}

impl AsRef<str> for AgentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for AgentId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for AgentId {
    fn from(id: &str) -> Self {
        Self(id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;

    #[test]
    fn agent_id_generation_creates_unique_ids() {
        let id1 = AgentId::generate();
        let id2 = AgentId::generate();
        assert_ne!(id1, id2);
        assert!(id1.as_str().starts_with("agent_"));
    }

    #[test]
    fn agent_id_from_delegate_name() {
        let id = AgentId::from_delegate_name("researcher");
        assert_eq!(id.as_str(), "delegate:researcher");
        assert!(id.is_delegate());
        assert_eq!(id.delegate_name(), Some("researcher"));
    }

    #[test]
    fn message_payload_from_string() {
        let payload: MessagePayload = "hello".into();
        assert_eq!(payload.as_text(), Some("hello"));
    }

    #[test]
    fn message_payload_from_json_value() {
        let value = serde_json::json!({"key": "value"});
        let payload: MessagePayload = value.clone().into();
        assert_eq!(payload.as_data(), Some(&value));
    }

    #[test]
    fn agent_message_request_creation() {
        let from = AgentId::new("agent_a".to_string());
        let to = AgentId::new("agent_b".to_string());
        let payload = MessagePayload::text("test message");

        let msg = AgentMessage::request(from.clone(), to.clone(), payload.clone());

        match msg {
            AgentMessage::Request { id: ref _id, from: ref f, to: ref t, payload: ref p, timestamp: _ts } => {
                assert!(!_id.is_empty());
                assert_eq!(f, &from);
                assert_eq!(t, &to);
                assert_eq!(p, &payload);
                assert!(_ts <= Utc::now() + chrono::Duration::seconds(1));
            }
            _ => panic!("Expected Request message"),
        }
    }

    #[test]
    fn agent_message_broadcast_creation() {
        let from = AgentId::new("agent_a".to_string());
        let payload = MessagePayload::text("broadcast test");

        let msg = AgentMessage::broadcast(from.clone(), payload.clone());

        match msg {
            AgentMessage::Broadcast { ref id, from: ref f, payload: ref p, .. } => {
                assert!(!id.is_empty());
                assert_eq!(f, &from);
                assert_eq!(p, &payload);
                assert!(msg.to().is_none());
            }
            _ => panic!("Expected Broadcast message"),
        }
    }

    #[test]
    fn agent_state_availability() {
        assert!(AgentState::Idle.is_available());
        assert!(!AgentState::Working.is_available());
        assert!(!AgentState::Error.is_available());
        assert!(!AgentState::ShuttingDown.is_available());
    }

    #[test]
    fn agent_message_response() {
        let request_id = "req_123".to_string();
        let from = AgentId::new("agent_b".to_string());
        let payload = MessagePayload::text("response");

        let msg = AgentMessage::response(request_id.clone(), from.clone(), payload.clone());

        match msg {
            AgentMessage::Response { request_id: ref rid, from: ref f, payload: ref p, .. } => {
                assert_eq!(rid, &request_id);
                assert_eq!(f, &from);
                assert_eq!(p, &payload);
            }
            _ => panic!("Expected Response message"),
        }
    }

    #[test]
    fn message_payload_task_delegation() {
        let payload = MessagePayload::task_delegation(
            "research this topic",
            Some("context".to_string()),
            Some("json".to_string()),
        );

        match payload {
            MessagePayload::TaskDelegation { prompt, context, expected_format } => {
                assert_eq!(prompt, "research this topic");
                assert_eq!(context, Some("context".to_string()));
                assert_eq!(expected_format, Some("json".to_string()));
            }
            _ => panic!("Expected TaskDelegation payload"),
        }
    }

    #[test]
    fn message_payload_status() {
        let mut metadata = HashMap::new();
        metadata.insert("task".to_string(), "research".to_string());

        let payload = MessagePayload::status(AgentState::Working, metadata.clone());

        match payload {
            MessagePayload::Status { state, metadata: m } => {
                assert_eq!(state, AgentState::Working);
                assert_eq!(m, metadata);
            }
            _ => panic!("Expected Status payload"),
        }
    }
}
