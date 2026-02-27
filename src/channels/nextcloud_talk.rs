use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// Nextcloud Talk channel in webhook mode.
///
/// Incoming messages are received by the gateway endpoint `/nextcloud-talk`.
/// Outbound replies are sent through Nextcloud Talk OCS API.
pub struct NextcloudTalkChannel {
    base_url: String,
    secret: String,
    allowed_users: Vec<String>,
    client: reqwest::Client,
}

impl NextcloudTalkChannel {
    pub fn new(base_url: String, secret: String, allowed_users: Vec<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            secret,
            allowed_users,
            client: reqwest::Client::new(),
        }
    }

    fn is_user_allowed(&self, actor_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == actor_id)
    }

    fn now_unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn parse_timestamp_secs(value: Option<&serde_json::Value>) -> u64 {
        let raw = match value {
            Some(serde_json::Value::Number(num)) => num.as_u64(),
            Some(serde_json::Value::String(s)) => s.trim().parse::<u64>().ok(),
            _ => None,
        }
        .unwrap_or_else(Self::now_unix_secs);

        // Some payloads use milliseconds.
        if raw > 1_000_000_000_000 {
            raw / 1000
        } else {
            raw
        }
    }

    fn value_to_string(value: Option<&serde_json::Value>) -> Option<String> {
        match value {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    /// Parse a Nextcloud Talk webhook payload into channel messages.
    ///
    /// Supports two formats:
    /// 1. Classic webhook: `type: "message"`, `message: { ... }`
    /// 2. Activity Streams 2.0 (Bot API): `type: "Create"`, `object: { ... }`, `target: { ... }`
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();
        tracing::debug!("Nextcloud Talk: processing webhook payload: {payload}");

        let Some(event_type) = payload.get("type").and_then(|v| v.as_str()) else {
            tracing::warn!("Nextcloud Talk: missing 'type' in webhook payload");
            return messages;
        };

        match event_type.to_lowercase().as_str() {
            "create" => {
                // Activity Streams 2.0 format (Bots)
                messages.extend(self.parse_as2_payload(payload));
            }
            "message" => {
                // Classic OCS message format
                messages.extend(self.parse_legacy_payload(payload));
            }
            _ => {
                tracing::debug!("Nextcloud Talk: skipping unknown event type: {event_type}");
            }
        }

        messages
    }

    fn parse_as2_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // Object can be a Note (chat message) or a Collection
        let Some(object) = payload.get("object") else {
            return messages;
        };

        let obj_type = object.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !obj_type.eq_ignore_ascii_case("Note") {
            tracing::debug!("Nextcloud Talk (AS2): skipping non-Note object type: {obj_type}");
            return messages;
        }

        // Room token is in target.id
        let room_token = payload
            .get("target")
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let Some(room_token) = room_token else {
            tracing::warn!("Nextcloud Talk (AS2): missing room token in 'target.id'");
            return messages;
        };

        // Actor ID is in actor.id (e.g. "users/ada-lovelace")
        let actor = payload.get("actor");
        let actor_type = actor.and_then(|a| a.get("type")).and_then(|v| v.as_str());
        let actor_id = actor
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .map(|id| {
                // Strip "users/" prefix if present
                id.strip_prefix("users/").unwrap_or(id)
            })
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let Some(actor_id) = actor_id else {
            tracing::warn!("Nextcloud Talk (AS2): missing actor ID");
            return messages;
        };

        // Skip bot-to-bot feedback loops
        if actor_type.is_some_and(|t| t.eq_ignore_ascii_case("Application")) {
            tracing::debug!("Nextcloud Talk (AS2): skipping application/bot actor: {actor_id}");
            return messages;
        }

        if !self.is_user_allowed(actor_id) {
            tracing::warn!("Nextcloud Talk: ignoring unauthorized actor: {actor_id}");
            return messages;
        }

        // object.content contains the message JSON string: {"message":"...", "parameters":{...}}
        let content_raw = object.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let content = if content_raw.starts_with('{') {
            // Try parse as JSON
            match serde_json::from_str::<serde_json::Value>(content_raw) {
                Ok(json) => json
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| content_raw.to_string()),
                Err(_) => content_raw.to_string(),
            }
        } else {
            content_raw.to_string()
        };

        if content.is_empty() {
            return messages;
        }

        let message_id = Self::value_to_string(object.get("id"))
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        
        let timestamp = Self::now_unix_secs(); // AS2 usually doesn't have it at top level, or it's 'published'

        messages.push(ChannelMessage {
            id: message_id,
            reply_target: room_token.to_string(),
            sender: actor_id.to_string(),
            content,
            channel: "nextcloud_talk".to_string(),
            timestamp,
            thread_ts: None,
        });

        messages
    }

    fn parse_legacy_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();
        let Some(message_obj) = payload.get("message") else {
            return messages;
        };

        let room_token = payload
            .get("object")
            .and_then(|obj| obj.get("token"))
            .and_then(|v| v.as_str())
            .or_else(|| message_obj.get("token").and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|token| !token.is_empty());

        let Some(room_token) = room_token else {
            tracing::warn!("Nextcloud Talk: missing room token in legacy payload");
            return messages;
        };

        let actor_type = message_obj
            .get("actorType")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("actorType").and_then(|v| v.as_str()))
            .unwrap_or("");

        if actor_type.eq_ignore_ascii_case("bots") {
            return messages;
        }

        let actor_id = message_obj
            .get("actorId")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("actorId").and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|id| !id.is_empty());

        let Some(actor_id) = actor_id else {
            return messages;
        };

        if !self.is_user_allowed(actor_id) {
            return messages;
        }

        let message_type = message_obj
            .get("messageType")
            .and_then(|v| v.as_str())
            .unwrap_or("comment");
        if !message_type.eq_ignore_ascii_case("comment") {
            return messages;
        }

        let has_system_message = message_obj
            .get("systemMessage")
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.trim().is_empty());
        if has_system_message {
            return messages;
        }

        let content = message_obj
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|c| !c.is_empty());

        let Some(content) = content else {
            return messages;
        };

        let message_id = Self::value_to_string(message_obj.get("id"))
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let timestamp = Self::parse_timestamp_secs(message_obj.get("timestamp"));

        messages.push(ChannelMessage {
            id: message_id,
            reply_target: room_token.to_string(),
            sender: actor_id.to_string(),
            content: content.to_string(),
            channel: "nextcloud_talk".to_string(),
            timestamp,
            thread_ts: None,
        });

        messages
    }

    async fn send_to_room(&self, room_token: &str, content: &str) -> anyhow::Result<()> {
        let encoded_room = urlencoding::encode(room_token);
        let url = format!(
            "{}/ocs/v2.php/apps/spreed/api/v1/bot/{}/message?format=json",
            self.base_url, encoded_room
        );

        let random_header = Uuid::new_v4().to_string().replace('-', "");
        let body = serde_json::json!({ "message": content });
        let body_str = body.to_string();
        
        // Calculate signature: hex(hmac_sha256(secret, random + body))
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
            .map_err(|e| anyhow::anyhow!("HMAC init failed: {e}"))?;
        mac.update(random_header.as_bytes());
        mac.update(body_str.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        tracing::debug!(
            "Nextcloud Talk: sending message to room {room_token} via Bot API. \
             URL: {url}, Random: {random_header}, Signature: {signature}"
        );

        let response = self
            .client
            .post(&url)
            .header("X-Nextcloud-Talk-Bot-Random", &random_header)
            .header("X-Nextcloud-Talk-Bot-Signature", &signature)
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await?;

        if response.status().is_success() {
            tracing::info!("Nextcloud Talk: successfully sent message to {room_token}");
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let sanitized = crate::providers::sanitize_api_error(&body);
        tracing::error!("Nextcloud Talk send failed: {status} — {sanitized}");
        anyhow::bail!("Nextcloud Talk Bot API error: {status}");
    }
}

#[async_trait]
impl Channel for NextcloudTalkChannel {
    fn name(&self) -> &str {
        "nextcloud_talk"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.send_to_room(&message.recipient, &message.content)
            .await
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!(
            "Nextcloud Talk channel active (webhook mode). \
            Configure Nextcloud Talk bot webhook to POST to your gateway's /nextcloud-talk endpoint."
        );

        // Keep task alive; incoming events are handled by the gateway webhook handler.
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/status.php", self.base_url);

        self.client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

/// Verify Nextcloud Talk webhook signature.
///
/// Signature calculation (official Talk bot docs):
/// `hex(hmac_sha256(secret, X-Nextcloud-Talk-Random + raw_body))`
pub fn verify_nextcloud_talk_signature(
    secret: &str,
    random: &str,
    body: &str,
    signature: &str,
) -> bool {
    let random = random.trim();
    if random.is_empty() {
        tracing::warn!("Nextcloud Talk: missing X-Nextcloud-Talk-Random header");
        return false;
    }

    let signature_hex = signature
        .trim()
        .strip_prefix("sha256=")
        .unwrap_or(signature)
        .trim();

    let Ok(provided) = hex::decode(signature_hex) else {
        tracing::warn!("Nextcloud Talk: invalid signature format");
        return false;
    };

    let payload = format!("{random}{body}");
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(payload.as_bytes());

    mac.verify_slice(&provided).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> NextcloudTalkChannel {
        NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "test-secret".into(),
            vec!["user_a".into()],
        )
    }

    #[test]
    fn nextcloud_talk_channel_name() {
        let channel = make_channel();
        assert_eq!(channel.name(), "nextcloud_talk");
    }

    #[test]
    fn nextcloud_talk_user_allowlist_exact_and_wildcard() {
        let channel = make_channel();
        assert!(channel.is_user_allowed("user_a"));
        assert!(!channel.is_user_allowed("user_b"));

        let wildcard = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "test-secret".into(),
            vec!["*".into()],
        );
        assert!(wildcard.is_user_allowed("any_user"));
    }

    #[test]
    fn nextcloud_talk_parse_valid_message_payload() {
        let channel = make_channel();
        let payload = serde_json::json!({
            "type": "message",
            "object": {
                "id": "42",
                "token": "room-token-123",
                "name": "Team Room",
                "type": "room"
            },
            "message": {
                "id": 77,
                "token": "room-token-123",
                "actorType": "users",
                "actorId": "user_a",
                "actorDisplayName": "User A",
                "timestamp": 1_735_701_200,
                "messageType": "comment",
                "systemMessage": "",
                "message": "Hello from Nextcloud"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "77");
        assert_eq!(messages[0].reply_target, "room-token-123");
        assert_eq!(messages[0].sender, "user_a");
        assert_eq!(messages[0].content, "Hello from Nextcloud");
        assert_eq!(messages[0].channel, "nextcloud_talk");
        assert_eq!(messages[0].timestamp, 1_735_701_200);
    }

    #[test]
    fn nextcloud_talk_parse_skips_non_message_events() {
        let channel = make_channel();
        let payload = serde_json::json!({
            "type": "room",
            "object": {"token": "room-token-123"},
            "message": {
                "actorType": "users",
                "actorId": "user_a",
                "message": "Hello"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(messages.is_empty());
    }

    #[test]
    fn nextcloud_talk_parse_skips_bot_messages() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "test-secret".into(),
            vec!["*".into()],
        );
        let payload = serde_json::json!({
            "type": "message",
            "object": {"token": "room-token-123"},
            "message": {
                "actorType": "bots",
                "actorId": "bot_1",
                "message": "Self message"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(messages.is_empty());
    }

    #[test]
    fn nextcloud_talk_parse_skips_unauthorized_sender() {
        let channel = make_channel();
        let payload = serde_json::json!({
            "type": "message",
            "object": {"token": "room-token-123"},
            "message": {
                "actorType": "users",
                "actorId": "user_b",
                "message": "Unauthorized"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(messages.is_empty());
    }

    #[test]
    fn nextcloud_talk_parse_skips_system_message() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "test-secret".into(),
            vec!["*".into()],
        );
        let payload = serde_json::json!({
            "type": "message",
            "object": {"token": "room-token-123"},
            "message": {
                "actorType": "users",
                "actorId": "user_a",
                "messageType": "comment",
                "systemMessage": "joined",
                "message": ""
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(messages.is_empty());
    }

    #[test]
    fn nextcloud_talk_parse_timestamp_millis_to_seconds() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "test-secret".into(),
            vec!["*".into()],
        );
        let payload = serde_json::json!({
            "type": "message",
            "object": {"token": "room-token-123"},
            "message": {
                "actorType": "users",
                "actorId": "user_a",
                "timestamp": 1_735_701_200_123_u64,
                "message": "hello"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].timestamp, 1_735_701_200);
    }

    #[test]
    fn nextcloud_talk_parse_as2_payload() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "test-secret".into(),
            vec!["*".into()],
        );
        let payload = serde_json::json!({
            "type": "Create",
            "actor": {
                "type": "Person",
                "id": "users/ada-lovelace",
                "name": "Ada Lovelace"
            },
            "object": {
                "type": "Note",
                "id": "1567",
                "name": "message",
                "content": "{\"message\":\"hi world !\",\"parameters\":{}}",
                "mediaType": "text/markdown"
            },
            "target": {
                "type": "Collection",
                "id": "n3xtc10ud",
                "name": "world"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "1567");
        assert_eq!(messages[0].reply_target, "n3xtc10ud");
        assert_eq!(messages[0].sender, "ada-lovelace");
        assert_eq!(messages[0].content, "hi world !");
        assert_eq!(messages[0].channel, "nextcloud_talk");
    }

    const TEST_WEBHOOK_SECRET: &str = "nextcloud_test_webhook_secret";

    #[test]
    fn nextcloud_talk_signature_verification_valid() {
        let secret = TEST_WEBHOOK_SECRET;
        let random = "random-seed";
        let body = r#"{"type":"message"}"#;

        let payload = format!("{random}{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        assert!(verify_nextcloud_talk_signature(
            secret, random, body, &signature
        ));
    }

    #[test]
    fn nextcloud_talk_signature_verification_invalid() {
        assert!(!verify_nextcloud_talk_signature(
            TEST_WEBHOOK_SECRET,
            "random-seed",
            r#"{"type":"message"}"#,
            "deadbeef"
        ));
    }

    #[test]
    fn nextcloud_talk_signature_verification_accepts_sha256_prefix() {
        let secret = TEST_WEBHOOK_SECRET;
        let random = "random-seed";
        let body = r#"{"type":"message"}"#;

        let payload = format!("{random}{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(verify_nextcloud_talk_signature(
            secret, random, body, &signature
        ));
    }
}
