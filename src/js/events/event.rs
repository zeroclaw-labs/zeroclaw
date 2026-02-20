// Event types for JS plugin hook system
//
// This module defines the core Event enum that represents different
// lifecycle and runtime events in ZeroClaw that plugins can hook into.
//
// # Security Considerations
//
// Events may carry sensitive data including:
// - User messages and content (MessageReceived)
// - Tool inputs and outputs (ToolCallPre, ToolCallPost)
// - LLM prompts and responses (LlmRequest)
//
// Plugin code receiving these events MUST NOT:
// - Log raw event payloads without sanitization
// - Expose event data to untrusted parties
// - Store secrets without encryption

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;

/// Core event types for JS plugin hook system
///
/// Events represent discrete lifecycle and runtime moments in ZeroClaw
/// that plugins can observe and react to. Each event carries context
/// specific to its type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "kebab-case")]
pub enum Event {
    /// Message received from a channel
    ///
    /// # Security Considerations
    /// - `message` may contain sensitive user data, PII, or secrets
    /// - Plugin code MUST NOT log raw message content
    /// - Consider sanitizing/redacting before logging or persisting
    MessageReceived {
        channel_id: String,
        channel_type: String,
        message: Value,
        session_id: Option<String>,
    },

    /// Tool execution about to occur
    ///
    /// # Security Considerations
    /// - `input` may contain sensitive parameters or secrets
    /// - Plugin code MUST NOT log raw tool inputs
    /// - Consider sanitizing/redacting before logging or persisting
    ToolCallPre {
        tool_name: String,
        input: Value,
        session_id: Option<String>,
    },

    /// Tool execution completed
    ///
    /// # Security Considerations
    /// - `result` may contain sensitive output data or secrets
    /// - Plugin code MUST NOT log raw tool outputs
    /// - Consider sanitizing/redacting before logging or persisting
    ToolCallPost {
        tool_name: String,
        result: Value,
        session_id: Option<String>,
    },

    /// LLM request being sent to provider
    ///
    /// # Security Considerations
    /// - `messages` may contain sensitive prompts, PII, or secrets
    /// - Plugin code MUST NOT log raw message content
    /// - Consider sanitizing/redacting before logging or persisting
    LlmRequest {
        provider: String,
        model: String,
        messages: Vec<Value>,
        options: Value,
    },

    SessionUpdate {
        session_id: String,
        context: Value,
    },

    BeforeAgentStart {
        config: Value,
    },

    /// Custom plugin-defined event
    ///
    /// # Constraints
    /// - `namespace`: must be non-empty, recommended format: `reverse.domain.name`
    /// - `name`: must be non-empty, alphanumeric with underscores/hyphens
    /// - `payload`: arbitrary JSON value
    Custom {
        namespace: String,
        name: String,
        payload: Value,
    },
}

impl Event {
    /// Returns the event name as a dotted string
    ///
    /// Used for event matching and hook registration.
    /// Returns `Cow<str>` to avoid allocations for static names.
    pub fn name(&self) -> Cow<str> {
        match self {
            Event::MessageReceived { .. } => Cow::Borrowed("message.received"),
            Event::ToolCallPre { .. } => Cow::Borrowed("tool.call.pre"),
            Event::ToolCallPost { .. } => Cow::Borrowed("tool.call.post"),
            Event::LlmRequest { .. } => Cow::Borrowed("llm.request"),
            Event::SessionUpdate { .. } => Cow::Borrowed("session.update"),
            Event::BeforeAgentStart { .. } => Cow::Borrowed("before.agent.start"),
            Event::Custom { name, .. } => Cow::Borrowed(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_name_message_received() {
        let event = Event::MessageReceived {
            channel_id: "123".to_string(),
            channel_type: "discord".to_string(),
            message: Value::Null,
            session_id: None,
        };
        assert_eq!(event.name().as_ref(), "message.received");
    }

    #[test]
    fn event_serialization() {
        let event = Event::MessageReceived {
            channel_id: "123".to_string(),
            channel_type: "discord".to_string(),
            message: serde_json::json!({"content": "hello"}),
            session_id: Some("session-abc".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "message.received");
    }
}
