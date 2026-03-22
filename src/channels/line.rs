use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::pairing;
use super::traits::{Channel, ChannelMessage, SendMessage};

/// LINE Messaging API channel.
///
/// Operates in webhook mode: incoming messages arrive via the gateway's
/// `/line` endpoint, while outgoing messages use the Push Message API.
pub struct LineChannel {
    channel_access_token: String,
    channel_secret: String,
    allowed_users: Vec<String>,
    client: reqwest::Client,
    pairing_store: Option<Arc<pairing::ChannelPairingStore>>,
    gateway_url: Option<String>,
}

impl LineChannel {
    pub fn new(
        channel_access_token: String,
        channel_secret: String,
        allowed_users: Vec<String>,
        pairing_store: Option<Arc<pairing::ChannelPairingStore>>,
        gateway_url: Option<String>,
    ) -> Self {
        Self {
            channel_access_token,
            channel_secret,
            allowed_users,
            client: reqwest::Client::new(),
            pairing_store,
            gateway_url,
        }
    }

    /// Check if a LINE user ID is in the allowlist.
    pub fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Verify the X-Line-Signature header using HMAC-SHA256.
    pub fn verify_signature(&self, body: &[u8], signature: &str) -> bool {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(self.channel_secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        let computed =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        computed == signature
    }

    /// Parse an incoming LINE webhook payload and extract text messages.
    ///
    /// Returns `(messages, unpaired_senders)`:
    /// - `messages`: authorized (or newly-paired) user messages
    /// - `unpaired_senders`: user IDs that need a one-click connect link
    pub fn parse_webhook_payload(
        &self,
        payload: &serde_json::Value,
    ) -> (Vec<ChannelMessage>, Vec<String>) {
        let mut messages = Vec::new();
        let mut unpaired = Vec::new();

        let Some(events) = payload.get("events").and_then(|e| e.as_array()) else {
            return (messages, unpaired);
        };

        for event in events {
            // Only process message events
            let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if event_type != "message" {
                continue;
            }

            // Only process text messages
            let msg = match event.get("message") {
                Some(m) => m,
                None => continue,
            };
            let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if msg_type != "text" {
                continue;
            }

            let text = msg
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                continue;
            }

            // Extract sender user ID
            let user_id = event
                .get("source")
                .and_then(|s| s.get("userId"))
                .and_then(|u| u.as_str())
                .unwrap_or("");
            if user_id.is_empty() {
                continue;
            }

            // Extract message ID
            let message_id = msg
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();

            // Extract timestamp (milliseconds)
            let timestamp = event
                .get("timestamp")
                .and_then(|t| t.as_u64())
                .map(|ms| ms / 1000) // Convert to seconds
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                });

            // Authorization check
            if !self.is_user_allowed(user_id) {
                // One-click auto-pair: check if user was paired via web flow
                if let Some(ref store) = self.pairing_store {
                    if store.is_paired("line", user_id) {
                        let uid = user_id.to_string();
                        tokio::spawn(async move {
                            tokio::task::spawn_blocking(move || {
                                if let Err(e) = pairing::persist_channel_allowlist("line", &uid) {
                                    tracing::error!("LINE: failed to persist pairing: {e}");
                                }
                            })
                            .await
                            .ok();
                        });
                        // Fall through to process message
                    } else {
                        // Collect for connect link sending
                        if !unpaired.contains(&user_id.to_string()) {
                            unpaired.push(user_id.to_string());
                        }
                        continue;
                    }
                } else {
                    tracing::warn!("LINE: ignoring message from unauthorized user: {user_id}");
                    continue;
                }
            }

            messages.push(ChannelMessage {
                id: message_id,
                sender: user_id.to_string(),
                reply_target: user_id.to_string(),
                content: text,
                channel: "line".to_string(),
                timestamp,
                thread_ts: None,
                silent: false,
            });
        }

        (messages, unpaired)
    }
}

#[async_trait]
impl Channel for LineChannel {
    fn name(&self) -> &str {
        "line"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // LINE Push Message API
        let url = "https://api.line.me/v2/bot/message/push";

        // Split long messages (LINE limit: 5000 chars per text message)
        let chunks = split_message(&message.content, 5000);

        // LINE allows max 5 messages per request
        for chunk_group in chunks.chunks(5) {
            let msg_objects: Vec<serde_json::Value> = chunk_group
                .iter()
                .map(|text| {
                    serde_json::json!({
                        "type": "text",
                        "text": text
                    })
                })
                .collect();

            let body = serde_json::json!({
                "to": message.recipient,
                "messages": msg_objects
            });

            let resp = self
                .client
                .post(url)
                .bearer_auth(&self.channel_access_token)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let error_body = resp.text().await.unwrap_or_default();
                tracing::error!("LINE send failed: {status} — {error_body}");
                anyhow::bail!("LINE API error: {status}");
            }
        }

        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // LINE uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /line endpoint.
        tracing::info!(
            "LINE channel active (webhook mode). \
            Configure LINE webhook to POST to your gateway's /line endpoint."
        );

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Verify the bot profile is accessible
        let resp = self
            .client
            .get("https://api.line.me/v2/bot/info")
            .bearer_auth(&self.channel_access_token)
            .send()
            .await;

        match resp {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

/// Split a message into chunks of at most `max_len` characters.
fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining);
            break;
        }
        // Try to split at a newline or space boundary
        let boundary = remaining[..max_len]
            .rfind('\n')
            .or_else(|| remaining[..max_len].rfind(' '))
            .unwrap_or(max_len);
        // Prevent infinite loop when no boundary is found at position 0
        let boundary = if boundary == 0 { max_len } else { boundary };
        let (chunk, rest) = remaining.split_at(boundary);
        chunks.push(chunk);
        remaining = rest.trim_start_matches('\n');
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> LineChannel {
        LineChannel::new(
            "test_token".into(),
            "test_secret".into(),
            vec!["+U1234567890".into()],
            None,
            None,
        )
    }

    fn make_wildcard_channel() -> LineChannel {
        LineChannel::new(
            "test_token".into(),
            "test_secret".into(),
            vec!["*".into()],
            None,
            None,
        )
    }

    #[test]
    fn line_user_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_user_allowed("+U1234567890"));
        assert!(!ch.is_user_allowed("+U9999999999"));
    }

    #[test]
    fn line_user_allowed_wildcard() {
        let ch = make_wildcard_channel();
        assert!(ch.is_user_allowed("+U1234567890"));
        assert!(ch.is_user_allowed("+Uanyone"));
    }

    #[test]
    fn line_user_denied_empty() {
        let ch = LineChannel::new("tok".into(), "sec".into(), vec![], None, None);
        assert!(!ch.is_user_allowed("+U1234567890"));
    }

    #[test]
    fn line_verify_signature_valid() {
        let ch = LineChannel::new("tok".into(), "my_channel_secret".into(), vec![], None, None);
        let body = b"test body content";
        // Compute expected signature
        let mut mac = Hmac::<Sha256>::new_from_slice(b"my_channel_secret").unwrap();
        mac.update(body);
        let expected =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert!(ch.verify_signature(body, &expected));
    }

    #[test]
    fn line_verify_signature_invalid() {
        let ch = LineChannel::new("tok".into(), "my_channel_secret".into(), vec![], None, None);
        assert!(!ch.verify_signature(b"body", "invalid_signature"));
    }

    #[test]
    fn line_parse_empty_payload() {
        let ch = make_channel();
        let payload = serde_json::json!({});
        let (msgs, unpaired) = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
        assert!(unpaired.is_empty());
    }

    #[test]
    fn line_parse_text_message_authorized() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "events": [{
                "type": "message",
                "message": {
                    "type": "text",
                    "id": "msg001",
                    "text": "Hello ZeroClaw"
                },
                "source": {
                    "type": "user",
                    "userId": "U1234"
                },
                "replyToken": "abc123",
                "timestamp": 1_700_000_000_000_u64
            }]
        });
        let (msgs, unpaired) = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "U1234");
        assert_eq!(msgs[0].content, "Hello ZeroClaw");
        assert_eq!(msgs[0].id, "msg001");
        assert_eq!(msgs[0].channel, "line");
        assert_eq!(msgs[0].timestamp, 1_700_000_000);
        assert!(unpaired.is_empty());
    }

    #[test]
    fn line_parse_unauthorized_user() {
        let ch = make_channel(); // Only allows "+U1234567890"
        let payload = serde_json::json!({
            "events": [{
                "type": "message",
                "message": {
                    "type": "text",
                    "id": "msg002",
                    "text": "Hi"
                },
                "source": {
                    "type": "user",
                    "userId": "U_unauthorized"
                },
                "replyToken": "abc123",
                "timestamp": 1_700_000_000_000_u64
            }]
        });
        let (msgs, unpaired) = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
        // No pairing store → user is just dropped (no unpaired tracking)
        assert!(unpaired.is_empty());
    }

    #[test]
    fn line_parse_non_text_message_skipped() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "events": [{
                "type": "message",
                "message": {
                    "type": "image",
                    "id": "msg003"
                },
                "source": {
                    "type": "user",
                    "userId": "U1234"
                },
                "replyToken": "abc123",
                "timestamp": 1_700_000_000_000_u64
            }]
        });
        let (msgs, _) = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn line_parse_non_message_event_skipped() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "events": [{
                "type": "follow",
                "source": {
                    "type": "user",
                    "userId": "U1234"
                },
                "replyToken": "abc123",
                "timestamp": 1_700_000_000_000_u64
            }]
        });
        let (msgs, _) = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn line_parse_empty_text_skipped() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "events": [{
                "type": "message",
                "message": {
                    "type": "text",
                    "id": "msg004",
                    "text": ""
                },
                "source": {
                    "type": "user",
                    "userId": "U1234"
                },
                "replyToken": "abc123",
                "timestamp": 1_700_000_000_000_u64
            }]
        });
        let (msgs, _) = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn line_parse_missing_user_id_skipped() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "events": [{
                "type": "message",
                "message": {
                    "type": "text",
                    "id": "msg005",
                    "text": "Hello"
                },
                "source": {
                    "type": "group"
                },
                "replyToken": "abc123",
                "timestamp": 1_700_000_000_000_u64
            }]
        });
        let (msgs, _) = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn line_parse_multiple_events() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "events": [
                {
                    "type": "message",
                    "message": { "type": "text", "id": "m1", "text": "First" },
                    "source": { "type": "user", "userId": "U1" },
                    "replyToken": "t1",
                    "timestamp": 1_700_000_000_000_u64
                },
                {
                    "type": "message",
                    "message": { "type": "text", "id": "m2", "text": "Second" },
                    "source": { "type": "user", "userId": "U2" },
                    "replyToken": "t2",
                    "timestamp": 1_700_000_001_000_u64
                }
            ]
        });
        let (msgs, _) = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "First");
        assert_eq!(msgs[1].content, "Second");
    }

    #[test]
    fn line_parse_events_not_array() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({ "events": "not_array" });
        let (msgs, _) = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn line_parse_no_message_field() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "events": [{
                "type": "message",
                "source": { "type": "user", "userId": "U1" },
                "replyToken": "t1",
                "timestamp": 1_700_000_000_000_u64
            }]
        });
        let (msgs, _) = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn line_split_message_short() {
        let chunks = split_message("hello", 100);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn line_split_message_exact_boundary() {
        let msg = "a".repeat(5000);
        let chunks = split_message(&msg, 5000);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn line_split_message_long() {
        let msg = "word ".repeat(2000); // 10000 chars
        let chunks = split_message(&msg, 5000);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 5000);
        }
    }

    #[test]
    fn line_channel_name() {
        let ch = make_channel();
        assert_eq!(Channel::name(&ch), "line");
    }
}
