use crate::security::OtpRequired;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// A pending OTP challenge with metadata needed by channel/gateway adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpPending {
    pub pending_id: String,
    pub channel: String,
    pub operator_id: String,
    pub action_details: OtpRequired,
    pub timeout_secs: u64,
    pub created_at_unix_secs: u64,
    pub expires_at_unix_secs: u64,
}

/// Successful OTP approval containing the original action context plus queued contexts.
#[derive(Debug, Clone)]
pub struct OtpApproved {
    pub pending: OtpPending,
    pub original_context: Value,
    pub queued_contexts: Vec<Value>,
}

/// OTP denial payload (invalid code, timeout, or other policy failure).
#[derive(Debug, Clone)]
pub struct OtpDenied {
    pub pending: Option<OtpPending>,
    pub reason: String,
    pub timed_out: bool,
    pub retryable: bool,
    pub original_context: Option<Value>,
    pub queued_contexts: Vec<Value>,
}

#[derive(Debug, Clone)]
struct PendingEntry {
    pending: OtpPending,
    original_context: Value,
    queued_contexts: VecDeque<Value>,
}

/// Shared, channel-agnostic OTP prompt/response coordinator.
///
/// The handler tracks pending OTP challenges keyed by `(channel, operator_id)`,
/// supports queueing non-OTP messages while authorization is pending, and
/// centralizes timeout + resolve flow for channels and gateway surfaces.
#[derive(Debug)]
pub struct OtpPromptHandler {
    timeout_secs: u64,
    entries: Mutex<HashMap<String, PendingEntry>>,
    by_operator: Mutex<HashMap<String, String>>,
}

impl OtpPromptHandler {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            timeout_secs: timeout_secs.max(1),
            entries: Mutex::new(HashMap::new()),
            by_operator: Mutex::new(HashMap::new()),
        }
    }

    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    pub fn prompt(
        &self,
        channel: impl Into<String>,
        operator_id: impl Into<String>,
        action_details: OtpRequired,
        original_context: Value,
    ) -> OtpPending {
        let channel = channel.into();
        let operator_id = operator_id.into();
        let operator_key = operator_key(&channel, &operator_id);
        let now = unix_now_secs();
        let pending = OtpPending {
            pending_id: Uuid::new_v4().to_string(),
            channel,
            operator_id,
            action_details,
            timeout_secs: self.timeout_secs,
            created_at_unix_secs: now,
            expires_at_unix_secs: now.saturating_add(self.timeout_secs),
        };

        if let Some(previous_id) = self
            .by_operator
            .lock()
            .insert(operator_key, pending.pending_id.clone())
        {
            self.entries.lock().remove(&previous_id);
        }

        self.entries.lock().insert(
            pending.pending_id.clone(),
            PendingEntry {
                pending: pending.clone(),
                original_context,
                queued_contexts: VecDeque::new(),
            },
        );

        pending
    }

    pub fn pending_for_operator(&self, channel: &str, operator_id: &str) -> Option<OtpPending> {
        let operator_key = operator_key(channel, operator_id);
        let pending_id = self.by_operator.lock().get(&operator_key).cloned()?;
        self.entries
            .lock()
            .get(&pending_id)
            .map(|entry| entry.pending.clone())
    }

    pub fn enqueue_non_otp(&self, pending_id: &str, context: Value) -> Result<usize, String> {
        let mut entries = self.entries.lock();
        let Some(entry) = entries.get_mut(pending_id) else {
            return Err("No pending OTP challenge found".to_string());
        };
        entry.queued_contexts.push_back(context);
        Ok(entry.queued_contexts.len())
    }

    /// Resolve an OTP challenge.
    ///
    /// `approve_fn` receives `(action_details, otp_code)` and should validate
    /// and apply approval side effects (e.g., security runtime cache updates).
    pub fn resolve<F>(
        &self,
        pending_id: &str,
        code: &str,
        mut approve_fn: F,
    ) -> Result<OtpApproved, OtpDenied>
    where
        F: FnMut(&OtpRequired, &str) -> Result<(), String>,
    {
        let Some(entry) = self.entries.lock().get(pending_id).cloned() else {
            return Err(OtpDenied {
                pending: None,
                reason: "No pending OTP challenge found".to_string(),
                timed_out: false,
                retryable: false,
                original_context: None,
                queued_contexts: Vec::new(),
            });
        };

        if is_expired(&entry.pending) {
            return Err(self
                .take_pending_as_timeout(pending_id)
                .unwrap_or_else(|| OtpDenied {
                    pending: Some(entry.pending),
                    reason: "OTP challenge timed out".to_string(),
                    timed_out: true,
                    retryable: false,
                    original_context: Some(entry.original_context),
                    queued_contexts: entry.queued_contexts.into_iter().collect(),
                }));
        }

        let normalized_code = code.trim();
        if !is_six_digit_otp(normalized_code) {
            return Err(OtpDenied {
                pending: Some(entry.pending),
                reason: "Invalid OTP format; expected a 6-digit code".to_string(),
                timed_out: false,
                retryable: true,
                original_context: None,
                queued_contexts: Vec::new(),
            });
        }

        if let Err(reason) = approve_fn(&entry.pending.action_details, normalized_code) {
            return Err(OtpDenied {
                pending: Some(entry.pending),
                reason,
                timed_out: false,
                retryable: true,
                original_context: None,
                queued_contexts: Vec::new(),
            });
        }

        let Some(removed) = self.take_pending(pending_id) else {
            return Err(OtpDenied {
                pending: None,
                reason: "Pending OTP challenge disappeared before approval".to_string(),
                timed_out: false,
                retryable: false,
                original_context: None,
                queued_contexts: Vec::new(),
            });
        };

        Ok(OtpApproved {
            pending: removed.pending,
            original_context: removed.original_context,
            queued_contexts: removed.queued_contexts.into_iter().collect(),
        })
    }

    pub fn timeout(&self, pending_id: &str) -> Option<OtpDenied> {
        self.take_pending_as_timeout(pending_id)
    }

    pub fn consume_expired_for_operator(
        &self,
        channel: &str,
        operator_id: &str,
    ) -> Option<OtpDenied> {
        let pending = self.pending_for_operator(channel, operator_id)?;
        if !is_expired(&pending) {
            return None;
        }
        self.take_pending_as_timeout(&pending.pending_id)
    }

    fn take_pending_as_timeout(&self, pending_id: &str) -> Option<OtpDenied> {
        self.take_pending(pending_id).map(|removed| OtpDenied {
            pending: Some(removed.pending),
            reason: "OTP challenge timed out".to_string(),
            timed_out: true,
            retryable: false,
            original_context: Some(removed.original_context),
            queued_contexts: removed.queued_contexts.into_iter().collect(),
        })
    }

    fn take_pending(&self, pending_id: &str) -> Option<PendingEntry> {
        let removed = self.entries.lock().remove(pending_id)?;
        let key = operator_key(&removed.pending.channel, &removed.pending.operator_id);
        let mut by_operator = self.by_operator.lock();
        if by_operator
            .get(&key)
            .is_some_and(|value| value == pending_id)
        {
            by_operator.remove(&key);
        }
        Some(removed)
    }
}

fn operator_key(channel: &str, operator_id: &str) -> String {
    format!("{channel}:{operator_id}")
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

fn is_expired(pending: &OtpPending) -> bool {
    unix_now_secs() > pending.expires_at_unix_secs
}

fn is_six_digit_otp(input: &str) -> bool {
    input.len() == 6 && input.chars().all(|ch| ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> OtpRequired {
        OtpRequired::for_tool("shell", "command=rm -rf ./build")
    }

    #[test]
    fn prompt_and_lookup_roundtrip() {
        let handler = OtpPromptHandler::new(120);
        let pending = handler.prompt(
            "telegram",
            "alice",
            payload(),
            serde_json::json!({"message": "deploy"}),
        );

        let found = handler
            .pending_for_operator("telegram", "alice")
            .expect("pending challenge should exist");
        assert_eq!(found.pending_id, pending.pending_id);
        assert_eq!(found.action_details.response_type, "otp_required");
    }

    #[test]
    fn resolve_invalid_format_is_retryable() {
        let handler = OtpPromptHandler::new(120);
        let pending = handler.prompt("discord", "u1", payload(), serde_json::json!({}));
        let denied = handler
            .resolve(&pending.pending_id, "abc", |_required, _code| Ok(()))
            .expect_err("invalid format should be denied");
        assert!(denied.retryable);
        assert!(!denied.timed_out);
    }

    #[test]
    fn resolve_success_returns_original_and_queue() {
        let handler = OtpPromptHandler::new(120);
        let pending = handler.prompt(
            "gateway_webhook",
            "client-a",
            payload(),
            serde_json::json!({"message": "open bank site"}),
        );
        handler
            .enqueue_non_otp(
                &pending.pending_id,
                serde_json::json!({"message": "queued-1"}),
            )
            .unwrap();
        handler
            .enqueue_non_otp(
                &pending.pending_id,
                serde_json::json!({"message": "queued-2"}),
            )
            .unwrap();

        let approved = handler
            .resolve(&pending.pending_id, "123456", |_required, _code| Ok(()))
            .expect("approval should succeed");
        assert_eq!(
            approved.original_context["message"].as_str(),
            Some("open bank site")
        );
        assert_eq!(approved.queued_contexts.len(), 2);
        assert!(handler
            .pending_for_operator("gateway_webhook", "client-a")
            .is_none());
    }

    #[test]
    fn timeout_removes_pending_and_returns_contexts() {
        let handler = OtpPromptHandler::new(120);
        let pending = handler.prompt(
            "telegram",
            "alice",
            payload(),
            serde_json::json!({"message": "build"}),
        );
        handler
            .enqueue_non_otp(
                &pending.pending_id,
                serde_json::json!({"message": "queued"}),
            )
            .unwrap();

        let denied = handler
            .timeout(&pending.pending_id)
            .expect("timeout should remove pending challenge");
        assert!(denied.timed_out);
        assert_eq!(
            denied
                .original_context
                .as_ref()
                .and_then(|v| v["message"].as_str()),
            Some("build")
        );
        assert_eq!(denied.queued_contexts.len(), 1);
    }
}
