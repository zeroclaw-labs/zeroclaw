//! Session message event types for async agent-to-agent messaging.
//!
//! Implements the receiver-discretionary session messaging contract from RFC #11027.
//! Messages are durable events with source/target verification, not direct history appends.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum UTF-8 byte length for message body (32 KiB).
pub const MAX_MESSAGE_BODY_BYTES: usize = 32 * 1024;

/// A durable peer-to-peer session message event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMessage {
    /// Unique delivery identifier (UUID v4).
    pub delivery_id: String,
    /// Source session key (verified from tool execution context).
    pub source_session_key: String,
    /// Target session key (verified to exist and be authorized).
    pub target_session_key: String,
    /// UTF-8 message body (max 32 KiB).
    pub body: String,
    /// When the message was accepted into the event stream.
    pub accepted_at: DateTime<Utc>,
    /// Source agent alias for attribution.
    pub source_agent_alias: Option<String>,
    /// Target agent alias for authorization.
    pub target_agent_alias: Option<String>,
    /// Whether the receiver has admitted this message for processing.
    pub admitted: bool,
    /// When the receiver admitted this message (if admitted).
    pub admitted_at: Option<DateTime<Utc>>,
}

impl SessionMessage {
    /// Create a new outbound session message.
    pub fn new(
        delivery_id: impl Into<String>,
        source_session_key: impl Into<String>,
        target_session_key: impl Into<String>,
        body: impl Into<String>,
        source_agent_alias: Option<String>,
        target_agent_alias: Option<String>,
    ) -> Self {
        Self {
            delivery_id: delivery_id.into(),
            source_session_key: source_session_key.into(),
            target_session_key: target_session_key.into(),
            body: body.into(),
            accepted_at: Utc::now(),
            source_agent_alias,
            target_agent_alias,
            admitted: false,
            admitted_at: None,
        }
    }

    /// Mark this message as admitted by the receiver.
    pub fn mark_admitted(&mut self) {
        self.admitted = true;
        self.admitted_at = Some(Utc::now());
    }

    /// Validate message body size.
    pub fn validate_body_size(body: &str) -> Result<(), MessageValidationError> {
        if body.len() > MAX_MESSAGE_BODY_BYTES {
            return Err(MessageValidationError::BodyTooLarge {
                actual: body.len(),
                max: MAX_MESSAGE_BODY_BYTES,
            });
        }
        Ok(())
    }
}

/// Errors that can occur during message validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageValidationError {
    /// Message body exceeds maximum size.
    BodyTooLarge { actual: usize, max: usize },
    /// Empty message body.
    EmptyBody,
    /// Invalid session key format.
    InvalidSessionKey { key: String },
}

impl fmt::Display for MessageValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BodyTooLarge { actual, max } => {
                write!(
                    f,
                    "Message body too large: {} bytes (max {} bytes)",
                    actual, max
                )
            }
            Self::EmptyBody => write!(f, "Message body cannot be empty"),
            Self::InvalidSessionKey { key } => {
                write!(f, "Invalid session key: '{}'", key)
            }
        }
    }
}

impl std::error::Error for MessageValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_message_has_delivery_id() {
        let msg = SessionMessage::new(
            "test-id",
            "source",
            "target",
            "body",
            Some("agent1".into()),
            Some("agent2".into()),
        );
        assert_eq!(msg.delivery_id, "test-id");
        assert_eq!(msg.source_session_key, "source");
        assert_eq!(msg.target_session_key, "target");
        assert_eq!(msg.body, "body");
        assert!(!msg.admitted);
        assert!(msg.admitted_at.is_none());
    }

    #[test]
    fn mark_admitted_sets_timestamp() {
        let mut msg = SessionMessage::new(
            "test-id",
            "source",
            "target",
            "body",
            None,
            None,
        );
        msg.mark_admitted();
        assert!(msg.admitted);
        assert!(msg.admitted_at.is_some());
    }

    #[test]
    fn validate_body_size_accepts_small_messages() {
        let body = "a".repeat(1000);
        assert!(SessionMessage::validate_body_size(&body).is_ok());
    }

    #[test]
    fn validate_body_size_rejects_large_messages() {
        let body = "a".repeat(MAX_MESSAGE_BODY_BYTES + 1);
        let err = SessionMessage::validate_body_size(&body).unwrap_err();
        assert!(matches!(err, MessageValidationError::BodyTooLarge { .. }));
    }

    #[test]
    fn validate_body_size_accepts_exactly_max() {
        let body = "a".repeat(MAX_MESSAGE_BODY_BYTES);
        assert!(SessionMessage::validate_body_size(&body).is_ok());
    }
}
