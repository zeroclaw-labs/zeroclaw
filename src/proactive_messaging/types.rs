use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A message queued for deferred delivery (e.g. during quiet hours).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: String,
    pub channel: String,
    pub recipient: String,
    pub message: String,
    pub priority: MessagePriority,
    pub reason_queued: Option<String>,
    /// Provenance tag, e.g. `"cron:<job-id> <name>"`.
    pub source_context: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: MessageStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessagePriority {
    Low,
    Normal,
    High,
    /// Urgent bypasses quiet hours (but NOT rate limits).
    Urgent,
}

impl MessagePriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }
}

impl fmt::Display for MessagePriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for MessagePriority {
    fn default() -> Self {
        Self::Normal
    }
}

impl std::str::FromStr for MessagePriority {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "normal" => Ok(Self::Normal),
            "high" => Ok(Self::High),
            "urgent" => Ok(Self::Urgent),
            other => Err(anyhow::anyhow!(
                "invalid priority '{other}': expected low, normal, high, or urgent"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    Pending,
    Sent,
    Expired,
    Cleared,
}

impl MessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Expired => "expired",
            Self::Cleared => "cleared",
        }
    }
}

impl fmt::Display for MessageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MessageStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "sent" => Ok(Self::Sent),
            "expired" => Ok(Self::Expired),
            "cleared" => Ok(Self::Cleared),
            other => Err(anyhow::anyhow!("invalid message status: '{other}'")),
        }
    }
}

/// Outcome of a guardrail evaluation.
#[derive(Debug, Clone)]
pub enum GuardrailDecision {
    Allowed,
    Denied(GuardrailDenialReason),
}

/// Why a proactive message was blocked.
#[derive(Debug, Clone)]
pub enum GuardrailDenialReason {
    QuietHours {
        window_end_description: String,
    },
    RateLimitHourly,
    RateLimitDaily,
    QueueFull,
    Disabled,
}

impl fmt::Display for GuardrailDenialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QuietHours {
                window_end_description,
            } => write!(f, "quiet hours (ends {window_end_description})"),
            Self::RateLimitHourly => f.write_str("hourly rate limit reached"),
            Self::RateLimitDaily => f.write_str("daily rate limit reached"),
            Self::QueueFull => f.write_str("outbound queue is full"),
            Self::Disabled => f.write_str("proactive messaging is disabled"),
        }
    }
}

/// The result of a `send_user_message` invocation.
#[derive(Debug, Clone)]
pub enum SendOutcome {
    Sent,
    Queued { reason: String },
    Denied { reason: String },
}
