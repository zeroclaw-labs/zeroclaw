use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use uuid::Uuid;

/// Linq channel — uses the Linq Partner V3 API for iMessage, RCS, and SMS.
///
/// This channel operates in webhook mode (push-based) rather than polling.
/// Messages are received via the gateway's `/linq` webhook endpoint.
/// The `listen` method here is a keepalive placeholder; actual message handling
/// happens in the gateway when Linq sends webhook events.
pub struct LinqChannel {
    api_token: String,
    from_phone: String,
    allowed_senders: Vec<String>,
    client: reqwest::Client,
}

const LINQ_API_BASE: &str = "https://api.linqapp.com/api/partner/v3";

impl LinqChannel {
    pub fn new(api_token: String, from_phone: String, allowed_senders: Vec<String>) -> Self {
        Self {
            api_token,
            from_phone,
            allowed_senders,
            client: reqwest::Client::new(),
        }
    }

    /// Check if a sender phone number is allowed (E.164 format: +1234567890)
    fn is_sender_allowed(&self, phone: &str) -> bool {
        self.allowed_senders.iter().any(|n| n == "*" || n == phone)
    }

    /// Get the bot's phone number
    pub fn phone_number(&self) -> &str {
        &self.from_phone
    }

    fn media_part_to_image_marker(part: &serde_json::Value) -> Option<String> {
        let source = part
            .get("url")
            .or_else(|| part.get("value"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())?;

        let mime_type = part
            .get("mime_type")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();

        if !mime_type.starts_with("image/") {
            return None;
        }

        Some(format!("[IMAGE:{source}]"))
    }

    /// Parse an incoming webhook payload from Linq and extract messages.
    ///
    /// Supports both legacy and v3 (2026-02-03) webhook payload formats.
    ///
    /// Legacy format:
    /// ```json
    /// {
    ///   "event_type": "message.received",
    ///   "data": {
    ///     "chat_id": "...",
    ///     "from": "+1...",
    ///     "is_from_me": false,
    ///     "message": { "parts": [{ "type": "text", "value": "..." }] }
    ///   }
    /// }
    /// ```
    ///
    /// V3 (2026-02-03) format:
    /// ```json
    /// {
    ///   "event_type": "message.received",
    ///   "data": {
    ///     "chat": { "id": "..." },
    ///     "sender_handle": { "handle": "+1...", "is_me": false },
    ///     "direction": "inbound",
    ///     "parts": [{ "type": "text", "value": "..." }]
    ///   }
    /// }
    /// ```
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // Only handle message.received events
        let event_type = payload
            .get("event_type")
            .and_then(|e| e.as_str())
            .unwrap_or("");
        if event_type != "message.received" {
            tracing::debug!("Linq: skipping non-message event: {event_type}");
            return messages;
        }

        let Some(data) = payload.get("data") else {
            return messages;
        };

        // Skip messages sent by the bot itself.
        // V3: data.sender_handle.is_me or data.direction == "outbound"
        // Legacy: data.is_from_me
        let is_from_me = data
            .get("sender_handle")
            .and_then(|sh| sh.get("is_me"))
            .and_then(|v| v.as_bool())
            .or_else(|| data.get("is_from_me").and_then(|v| v.as_bool()))
            .unwrap_or(false);
        let is_outbound = data
            .get("direction")
            .and_then(|d| d.as_str())
            .is_some_and(|d| d == "outbound");
        if is_from_me || is_outbound {
            tracing::debug!("Linq: skipping outbound/is_from_me message");
            return messages;
        }

        // Get sender phone number.
        // V3: data.sender_handle.handle
        // Legacy: data.from
        let from = data
            .get("sender_handle")
            .and_then(|sh| sh.get("handle"))
            .and_then(|h| h.as_str())
            .or_else(|| data.get("from").and_then(|f| f.as_str()));
        let Some(from) = from else {
            tracing::debug!("Linq: no sender phone found in payload");
            return messages;
        };

        // Normalize to E.164 format
        let normalized_from = if from.starts_with('+') {
            from.to_string()
        } else {
            format!("+{from}")
        };

        // Check allowlist
        if !self.is_sender_allowed(&normalized_from) {
            tracing::warn!(
                "Linq: ignoring message from unauthorized sender: {normalized_from}. \
                Add to channels.linq.allowed_senders in config.toml, \
                or run `zeroclaw onboard --channels-only` to configure interactively."
            );
            return messages;
        }

        // Get chat_id for reply routing.
        // V3: data.chat.id
        // Legacy: data.chat_id
        let chat_id = data
            .get("chat")
            .and_then(|c| c.get("id"))
            .and_then(|id| id.as_str())
            .or_else(|| data.get("chat_id").and_then(|c| c.as_str()))
            .unwrap_or("")
            .to_string();

        // Extract message parts.
        // V3: data.parts (directly under data)
        // Legacy: data.message.parts
        let parts = data
            .get("parts")
            .and_then(|p| p.as_array())
            .or_else(|| {
                data.get("message")
                    .and_then(|m| m.get("parts"))
                    .and_then(|p| p.as_array())
            });
        let Some(parts) = parts else {
            tracing::debug!("Linq: no message parts found in payload");
            return messages;
        };

        let content_parts: Vec<String> = parts
            .iter()
            .filter_map(|part| {
                let part_type = part.get("type").and_then(|t| t.as_str())?;
                match part_type {
                    "text" => part
                        .get("value")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string),
                    "media" | "image" => {
                        if let Some(marker) = Self::media_part_to_image_marker(part) {
                            Some(marker)
                        } else {
                            tracing::debug!("Linq: skipping unsupported {part_type} part");
                            None
                        }
                    }
                    _ => {
                        tracing::debug!("Linq: skipping {part_type} part");
                        None
                    }
                }
            })
            .collect();

        if content_parts.is_empty() {
            return messages;
        }

        let content = content_parts.join("\n").trim().to_string();

        if content.is_empty() {
            return messages;
        }

        // Get timestamp from created_at or use current time
        let timestamp = payload
            .get("created_at")
            .and_then(|t| t.as_str())
            .and_then(|t| {
                chrono::DateTime::parse_from_rfc3339(t)
                    .ok()
                    .map(|dt| dt.timestamp().cast_unsigned())
            })
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            });

        // Use chat_id as reply_target so replies go to the right conversation
        let reply_target = if chat_id.is_empty() {
            normalized_from.clone()
        } else {
            chat_id
        };

        messages.push(ChannelMessage {
            id: Uuid::new_v4().to_string(),
            reply_target,
            sender: normalized_from,
            content,
            channel: "linq".to_string(),
            timestamp,
            thread_ts: None,
        });

        messages
    }
}

#[async_trait]
impl Channel for LinqChannel {
    fn name(&self) -> &str {
        "linq"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // If reply_target looks like a chat_id, send to existing chat.
        // Otherwise create a new chat with the recipient phone number.
        let recipient = &message.recipient;

        let body = serde_json::json!({
            "message": {
                "parts": [{
                    "type": "text",
                    "value": message.content
                }]
            }
        });

        // Try sending to existing chat (recipient is chat_id)
        let url = format!("{LINQ_API_BASE}/chats/{recipient}/messages");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(());
        }

        // If the chat_id-based send failed with 404, try creating a new chat
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let new_chat_body = serde_json::json!({
                "from": self.from_phone,
                "to": [recipient],
                "message": {
                    "parts": [{
                        "type": "text",
                        "value": message.content
                    }]
                }
            });

            let create_resp = self
                .client
                .post(format!("{LINQ_API_BASE}/chats"))
                .bearer_auth(&self.api_token)
                .header("Content-Type", "application/json")
                .json(&new_chat_body)
                .send()
                .await?;

            if !create_resp.status().is_success() {
                let status = create_resp.status();
                let error_body = create_resp.text().await.unwrap_or_default();
                let sanitized = crate::providers::sanitize_api_error(&error_body);
                tracing::error!("Linq create chat failed: {status} — {sanitized}");
                anyhow::bail!("Linq API error: {status}");
            }

            return Ok(());
        }

        let status = resp.status();
        let error_body = resp.text().await.unwrap_or_default();
        let sanitized = crate::providers::sanitize_api_error(&error_body);
        tracing::error!("Linq send failed: {status} — {sanitized}");
        anyhow::bail!("Linq API error: {status}");
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Linq uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /linq endpoint.
        tracing::info!(
            "Linq channel active (webhook mode). \
            Configure Linq webhook to POST to your gateway's /linq endpoint."
        );

        // Keep the task alive — it will be cancelled when the channel shuts down
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Check if we can reach the Linq API
        let url = format!("{LINQ_API_BASE}/phonenumbers");

        self.client
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let url = format!("{LINQ_API_BASE}/chats/{recipient}/typing");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::debug!("Linq start_typing failed: {}", resp.status());
        }

        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let url = format!("{LINQ_API_BASE}/chats/{recipient}/typing");

        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::debug!("Linq stop_typing failed: {}", resp.status());
        }

        Ok(())
    }
}

/// Verify a Linq webhook signature.
///
/// Linq signs webhooks with HMAC-SHA256 over `"{timestamp}.{body}"`.
/// The signature is sent in `X-Webhook-Signature` (hex-encoded) and the
/// timestamp in `X-Webhook-Timestamp`. Reject timestamps older than 300s.
pub fn verify_linq_signature(secret: &str, body: &str, timestamp: &str, signature: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    // Reject stale timestamps (>300s old)
    if let Ok(ts) = timestamp.parse::<i64>() {
        let now = chrono::Utc::now().timestamp();
        if (now - ts).unsigned_abs() > 300 {
            tracing::warn!("Linq: rejecting stale webhook timestamp ({ts}, now={now})");
            return false;
        }
    } else {
        tracing::warn!("Linq: invalid webhook timestamp: {timestamp}");
        return false;
    }

    // Compute HMAC-SHA256 over "{timestamp}.{body}"
    let message = format!("{timestamp}.{body}");
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(message.as_bytes());
    let signature_hex = signature
        .trim()
        .strip_prefix("sha256=")
        .unwrap_or(signature);
    let Ok(provided) = hex::decode(signature_hex.trim()) else {
        tracing::warn!("Linq: invalid webhook signature format");
        return false;
    };

    // Constant-time comparison via HMAC verify.
    mac.verify_slice(&provided).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> LinqChannel {
        LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
    }

    #[test]
    fn linq_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "linq");
    }

    #[test]
    fn linq_sender_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_sender_allowed("+1234567890"));
        assert!(!ch.is_sender_allowed("+9876543210"));
    }

    #[test]
    fn linq_sender_allowed_wildcard() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        assert!(ch.is_sender_allowed("+1234567890"));
        assert!(ch.is_sender_allowed("+9999999999"));
    }

    #[test]
    fn linq_sender_allowed_empty() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec![]);
        assert!(!ch.is_sender_allowed("+1234567890"));
    }

    #[test]
    fn linq_parse_valid_text_message() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "api_version": "v3",
            "event_type": "message.received",
            "event_id": "evt-123",
            "created_at": "2025-01-15T12:00:00Z",
            "trace_id": "trace-456",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "recipient_phone": "+15551234567",
                "is_from_me": false,
                "service": "iMessage",
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "text",
                        "value": "Hello ZeroClaw!"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello ZeroClaw!");
        assert_eq!(msgs[0].channel, "linq");
        assert_eq!(msgs[0].reply_target, "chat-789");
    }

    #[test]
    fn linq_parse_skip_is_from_me() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": true,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "My own message" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "is_from_me messages should be skipped");
    }

    #[test]
    fn linq_parse_skip_non_message_event() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "event_type": "message.delivered",
            "data": {
                "chat_id": "chat-789",
                "message_id": "msg-abc"
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Non-message events should be skipped");
    }

    #[test]
    fn linq_parse_unauthorized_sender() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+9999999999",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "Spam" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Unauthorized senders should be filtered");
    }

    #[test]
    fn linq_parse_empty_payload() {
        let ch = make_channel();
        let payload = serde_json::json!({});
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn linq_parse_media_only_translated_to_image_marker() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "media",
                        "url": "https://example.com/image.jpg",
                        "mime_type": "image/jpeg"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[IMAGE:https://example.com/image.jpg]");
    }

    #[test]
    fn linq_parse_media_non_image_still_skipped() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "media",
                        "url": "https://example.com/sound.mp3",
                        "mime_type": "audio/mpeg"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Non-image media should still be skipped");
    }

    #[test]
    fn linq_parse_multiple_text_parts() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [
                        { "type": "text", "value": "First part" },
                        { "type": "text", "value": "Second part" }
                    ]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "First part\nSecond part");
    }

    /// Fixture secret used exclusively in signature-verification unit tests (not a real credential).
    const TEST_WEBHOOK_SECRET: &str = "test_webhook_secret";

    #[test]
    fn linq_signature_verification_valid() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        let now = chrono::Utc::now().timestamp().to_string();

        // Compute expected signature
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let message = format!("{now}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        assert!(verify_linq_signature(secret, body, &now, &signature));
    }

    #[test]
    fn linq_signature_verification_invalid() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        let now = chrono::Utc::now().timestamp().to_string();

        assert!(!verify_linq_signature(
            secret,
            body,
            &now,
            "deadbeefdeadbeefdeadbeef"
        ));
    }

    #[test]
    fn linq_signature_verification_stale_timestamp() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        // 10 minutes ago — stale
        let stale_ts = (chrono::Utc::now().timestamp() - 600).to_string();

        // Even with correct signature, stale timestamp should fail
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let message = format!("{stale_ts}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        assert!(
            !verify_linq_signature(secret, body, &stale_ts, &signature),
            "Stale timestamps (>300s) should be rejected"
        );
    }

    #[test]
    fn linq_signature_verification_accepts_sha256_prefix() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        let now = chrono::Utc::now().timestamp().to_string();

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let message = format!("{now}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(verify_linq_signature(secret, body, &now, &signature));
    }

    #[test]
    fn linq_signature_verification_accepts_uppercase_hex() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        let now = chrono::Utc::now().timestamp().to_string();

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let message = format!("{now}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes()).to_ascii_uppercase();

        assert!(verify_linq_signature(secret, body, &now, &signature));
    }

    #[test]
    fn linq_parse_normalizes_phone_with_plus() {
        let ch = LinqChannel::new(
            "tok".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        );
        // API sends without +, normalize to +
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "Hi" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn linq_parse_missing_data() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "event_type": "message.received"
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn linq_parse_missing_message_parts() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc"
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn linq_parse_empty_text_value() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Empty text should be skipped");
    }

    #[test]
    fn linq_parse_fallback_reply_target_when_no_chat_id() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "Hi" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        // Falls back to sender phone number when no chat_id
        assert_eq!(msgs[0].reply_target, "+1234567890");
    }

    #[test]
    fn linq_phone_number_accessor() {
        let ch = make_channel();
        assert_eq!(ch.phone_number(), "+15551234567");
    }

    // --- V3 (2026-02-03) payload format tests ---

    #[test]
    fn linq_parse_v3_text_message() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "api_version": "v3",
            "webhook_version": "2026-02-03",
            "event_type": "message.received",
            "event_id": "da0e9e98-1259-4f2e-be61-981013fb2e72",
            "created_at": "2026-02-25T19:04:25.843133967Z",
            "trace_id": "e3d445014dce473e75a99ca147e31242",
            "partner_id": "1fdfb6c9-c237-5aaf-8e03-9fce619289f9",
            "data": {
                "chat": {
                    "id": "28de5f27-e8c4-4d45-a052-4d9ad7d77ef5",
                    "is_group": false,
                    "owner_handle": {
                        "handle": "+15551234567",
                        "id": "823f96c0-810f-47d0-b7fe-7c45a8ac2c2b",
                        "is_me": true,
                        "joined_at": "2026-02-25T17:29:31.050207Z",
                        "left_at": null,
                        "service": "iMessage",
                        "status": "active"
                    }
                },
                "direction": "inbound",
                "id": "8ddb37aa-968f-4815-bfe3-370d366159b8",
                "parts": [
                    { "type": "text", "value": "Hello ZeroClaw!" }
                ],
                "sender_handle": {
                    "handle": "+1234567890",
                    "id": "6d80f902-be03-4e47-ae20-bf69b4ed4b3c",
                    "is_me": false,
                    "joined_at": "2026-02-25T17:29:31.050207Z",
                    "left_at": null,
                    "service": "iMessage",
                    "status": "active"
                },
                "sent_at": "2026-02-25T19:04:25.361Z",
                "service": "iMessage"
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello ZeroClaw!");
        assert_eq!(msgs[0].channel, "linq");
        assert_eq!(msgs[0].reply_target, "28de5f27-e8c4-4d45-a052-4d9ad7d77ef5");
    }

    #[test]
    fn linq_parse_v3_skip_outbound() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat": { "id": "chat-v3" },
                "direction": "outbound",
                "sender_handle": {
                    "handle": "+15551234567",
                    "is_me": true
                },
                "parts": [{ "type": "text", "value": "My own message" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "outbound/is_me messages should be skipped in v3 format");
    }

    #[test]
    fn linq_parse_v3_skip_is_me() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat": { "id": "chat-v3" },
                "direction": "inbound",
                "sender_handle": {
                    "handle": "+15551234567",
                    "is_me": true
                },
                "parts": [{ "type": "text", "value": "Echoed message" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "sender_handle.is_me should be skipped in v3 format");
    }

    #[test]
    fn linq_parse_v3_unauthorized_sender() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat": { "id": "chat-v3" },
                "direction": "inbound",
                "sender_handle": {
                    "handle": "+9999999999",
                    "is_me": false
                },
                "parts": [{ "type": "text", "value": "Spam" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Unauthorized senders should be filtered in v3 format");
    }

    #[test]
    fn linq_parse_v3_multiple_parts() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat": { "id": "chat-v3" },
                "direction": "inbound",
                "sender_handle": { "handle": "+1234567890", "is_me": false },
                "parts": [
                    { "type": "text", "value": "First" },
                    { "type": "text", "value": "Second" }
                ]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "First\nSecond");
    }
}
