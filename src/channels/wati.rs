use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use uuid::Uuid;

/// WATI WhatsApp Business API channel.
///
/// This channel operates in webhook mode (push-based) rather than polling.
/// Messages are received via the gateway's `/wati` webhook endpoint.
/// The `listen` method here is a keepalive placeholder; actual message handling
/// happens in the gateway when WATI sends webhook events.
pub struct WatiChannel {
    api_token: String,
    api_url: String,
    tenant_id: Option<String>,
    allowed_numbers: Vec<String>,
    client: reqwest::Client,
}

impl WatiChannel {
    pub fn new(
        api_token: String,
        api_url: String,
        tenant_id: Option<String>,
        allowed_numbers: Vec<String>,
    ) -> Self {
        Self {
            api_token,
            api_url,
            tenant_id,
            allowed_numbers,
            client: crate::config::build_runtime_proxy_client("channel.wati"),
        }
    }

    /// Check if a phone number is allowed (E.164 format: +1234567890).
    fn is_number_allowed(&self, phone: &str) -> bool {
        self.allowed_numbers.iter().any(|n| n == "*" || n == phone)
    }

    /// Build the target field for the WATI API, prefixing with tenant_id if set.
    fn build_target(&self, phone: &str) -> String {
        // Strip leading '+' — WATI expects bare digits
        let bare = phone.strip_prefix('+').unwrap_or(phone);
        if let Some(ref tid) = self.tenant_id {
            if bare.starts_with(&format!("{tid}:")) {
                bare.to_string()
            } else {
                format!("{tid}:{bare}")
            }
        } else {
            bare.to_string()
        }
    }

    /// Parse an incoming webhook payload from WATI and extract messages.
    ///
    /// WATI's webhook payloads have variable field names depending on the API
    /// version and configuration, so we try multiple paths for each field.
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // Extract text — try multiple field paths
        let text = payload
            .get("text")
            .and_then(|v| v.as_str())
            .or_else(|| {
                payload
                    .get("message")
                    .and_then(|m| m.get("text").or_else(|| m.get("body")))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("")
            .trim();

        if text.is_empty() {
            return messages;
        }

        // Check fromMe — skip outgoing messages
        let from_me = payload
            .get("fromMe")
            .or_else(|| payload.get("from_me"))
            .or_else(|| payload.get("owner"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if from_me {
            tracing::debug!("WATI: skipping fromMe message");
            return messages;
        }

        // Extract waId (sender phone number)
        let wa_id = payload
            .get("waId")
            .or_else(|| payload.get("wa_id"))
            .or_else(|| payload.get("from"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if wa_id.is_empty() {
            return messages;
        }

        // Normalize phone to E.164 format
        let normalized_phone = if wa_id.starts_with('+') {
            wa_id.to_string()
        } else {
            format!("+{wa_id}")
        };

        // Check allowlist
        if !self.is_number_allowed(&normalized_phone) {
            tracing::warn!(
                "WATI: ignoring message from unauthorized sender: {normalized_phone}. \
                Add to channels.wati.allowed_numbers in config.toml, \
                or run `zeroclaw onboard --channels-only` to configure interactively."
            );
            return messages;
        }

        // Extract timestamp — handle unix seconds, unix ms, or ISO string
        let timestamp = payload
            .get("timestamp")
            .or_else(|| payload.get("created"))
            .map(|t| {
                if let Some(secs) = t.as_u64() {
                    // Distinguish seconds from milliseconds (ms > 10_000_000_000)
                    if secs > 10_000_000_000 {
                        secs / 1000
                    } else {
                        secs
                    }
                } else if let Some(s) = t.as_str() {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp().cast_unsigned())
                        .unwrap_or_else(|| {
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                        })
                } else {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                }
            })
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            });

        messages.push(ChannelMessage {
            id: Uuid::new_v4().to_string(),
            reply_target: normalized_phone.clone(),
            sender: normalized_phone,
            content: text.to_string(),
            channel: "wati".to_string(),
            timestamp,
            thread_ts: None,
        });

        messages
    }
}

#[async_trait]
impl Channel for WatiChannel {
    fn name(&self) -> &str {
        "wati"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let target = self.build_target(&message.recipient);

        let body = serde_json::json!({
            "target": target,
            "text": message.content
        });

        let url = format!("{}/api/ext/v3/conversations/messages/text", self.api_url);

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            tracing::error!("WATI send failed: {status} — {error_body}");
            anyhow::bail!("WATI API error: {status}");
        }

        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // WATI uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /wati endpoint.
        tracing::info!(
            "WATI channel active (webhook mode). \
            Configure WATI webhook to POST to your gateway's /wati endpoint."
        );

        // Keep the task alive — it will be cancelled when the channel shuts down
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/api/ext/v3/contacts/count", self.api_url);

        self.client
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // WATI API does not support typing indicators
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // WATI API does not support typing indicators
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> WatiChannel {
        WatiChannel {
            api_token: "test-token".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: None,
            allowed_numbers: vec!["+1234567890".into()],
            client: reqwest::Client::new(),
        }
    }

    fn make_wildcard_channel() -> WatiChannel {
        WatiChannel {
            api_token: "test-token".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: None,
            allowed_numbers: vec!["*".into()],
            client: reqwest::Client::new(),
        }
    }

    #[test]
    fn wati_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "wati");
    }

    #[test]
    fn wati_number_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(!ch.is_number_allowed("+9876543210"));
    }

    #[test]
    fn wati_number_allowed_wildcard() {
        let ch = make_wildcard_channel();
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(ch.is_number_allowed("+9999999999"));
    }

    #[test]
    fn wati_number_allowed_empty() {
        let ch = WatiChannel {
            api_token: "tok".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: None,
            allowed_numbers: vec![],
            client: reqwest::Client::new(),
        };
        assert!(!ch.is_number_allowed("+1234567890"));
    }

    #[test]
    fn wati_build_target_with_tenant() {
        let ch = WatiChannel {
            api_token: "tok".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: Some("tenant1".into()),
            allowed_numbers: vec![],
            client: reqwest::Client::new(),
        };
        assert_eq!(ch.build_target("+1234567890"), "tenant1:1234567890");
    }

    #[test]
    fn wati_build_target_without_tenant() {
        let ch = make_channel();
        assert_eq!(ch.build_target("+1234567890"), "1234567890");
    }

    #[test]
    fn wati_build_target_already_prefixed() {
        let ch = WatiChannel {
            api_token: "tok".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: Some("tenant1".into()),
            allowed_numbers: vec![],
            client: reqwest::Client::new(),
        };
        // If the phone already has the tenant prefix, don't double it
        assert_eq!(ch.build_target("tenant1:1234567890"), "tenant1:1234567890");
    }

    #[test]
    fn wati_parse_valid_message() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "text": "Hello from WATI!",
            "waId": "1234567890",
            "fromMe": false,
            "timestamp": 1705320000u64
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello from WATI!");
        assert_eq!(msgs[0].channel, "wati");
        assert_eq!(msgs[0].reply_target, "+1234567890");
        assert_eq!(msgs[0].timestamp, 1705320000);
    }

    #[test]
    fn wati_parse_skip_from_me() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "text": "My own message",
            "waId": "1234567890",
            "fromMe": true
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "fromMe messages should be skipped");
    }

    #[test]
    fn wati_parse_skip_no_text() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "waId": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Messages without text should be skipped");
    }

    #[test]
    fn wati_parse_alternative_field_names() {
        let ch = make_wildcard_channel();

        // wa_id instead of waId, message.body instead of text
        let payload = serde_json::json!({
            "message": { "body": "Alt field test" },
            "wa_id": "1234567890",
            "from_me": false,
            "timestamp": 1705320000u64
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Alt field test");
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn wati_parse_timestamp_seconds() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "text": "Test",
            "waId": "1234567890",
            "timestamp": 1705320000u64
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].timestamp, 1705320000);
    }

    #[test]
    fn wati_parse_timestamp_milliseconds() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "text": "Test",
            "waId": "1234567890",
            "timestamp": 1705320000000u64
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].timestamp, 1705320000);
    }

    #[test]
    fn wati_parse_timestamp_iso() {
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "text": "Test",
            "waId": "1234567890",
            "timestamp": "2025-01-15T12:00:00Z"
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].timestamp, 1736942400);
    }

    #[test]
    fn wati_parse_normalizes_phone() {
        let ch = WatiChannel {
            api_token: "tok".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: None,
            allowed_numbers: vec!["+1234567890".into()],
            client: reqwest::Client::new(),
        };

        // Phone without + prefix
        let payload = serde_json::json!({
            "text": "Hi",
            "waId": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn wati_parse_empty_payload() {
        let ch = make_channel();
        let payload = serde_json::json!({});
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn wati_parse_from_field_fallback() {
        let ch = make_wildcard_channel();
        // Uses "from" instead of "waId"
        let payload = serde_json::json!({
            "text": "Fallback test",
            "from": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn wati_parse_message_text_fallback() {
        let ch = make_wildcard_channel();
        // Uses "message.text" instead of top-level "text"
        let payload = serde_json::json!({
            "message": { "text": "Nested text" },
            "waId": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Nested text");
    }

    #[test]
    fn wati_parse_owner_field_as_from_me() {
        let ch = make_wildcard_channel();
        // Uses "owner" field as fromMe indicator
        let payload = serde_json::json!({
            "text": "Test",
            "waId": "1234567890",
            "owner": true
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "owner=true messages should be skipped");
    }
}
