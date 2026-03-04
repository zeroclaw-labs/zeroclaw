use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use uuid::Uuid;

/// Nextcloud Talk channel in webhook mode.
///
/// Incoming messages are received by the gateway endpoint `/nextcloud-talk`.
/// Outbound replies are sent through Nextcloud Talk OCS API.
pub struct NextcloudTalkChannel {
    base_url: String,
    app_token: String,
    allowed_users: Vec<String>,
    client: reqwest::Client,
}

impl NextcloudTalkChannel {
    pub fn new(base_url: String, app_token: String, allowed_users: Vec<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            app_token,
            allowed_users,
            client: reqwest::Client::new(),
        }
    }

    fn canonical_actor_id(actor_id: &str) -> &str {
        let trimmed = actor_id.trim();
        trimmed.rsplit('/').next().unwrap_or(trimmed)
    }

    fn is_user_allowed(&self, actor_id: &str) -> bool {
        let actor_id = actor_id.trim();
        if actor_id.is_empty() {
            return false;
        }

        if self.allowed_users.iter().any(|u| u == "*") {
            return true;
        }

        let actor_short = Self::canonical_actor_id(actor_id);
        self.allowed_users.iter().any(|allowed| {
            let allowed = allowed.trim();
            if allowed.is_empty() {
                return false;
            }
            let allowed_short = Self::canonical_actor_id(allowed);
            allowed.eq_ignore_ascii_case(actor_id)
                || allowed.eq_ignore_ascii_case(actor_short)
                || allowed_short.eq_ignore_ascii_case(actor_id)
                || allowed_short.eq_ignore_ascii_case(actor_short)
        })
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

    fn extract_content_from_as2_object(payload: &serde_json::Value) -> Option<String> {
        let Some(content_value) = payload.get("object").and_then(|obj| obj.get("content")) else {
            return None;
        };

        let content = match content_value {
            serde_json::Value::String(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return None;
                }

                // Activity Streams payloads often embed message text as JSON inside object.content.
                if let Ok(decoded) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if let Some(message) = decoded.get("message").and_then(|v| v.as_str()) {
                        let message = message.trim();
                        if !message.is_empty() {
                            return Some(message.to_string());
                        }
                    }
                }

                trimmed.to_string()
            }
            serde_json::Value::Object(map) => map
                .get("message")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(ToOwned::to_owned)?,
            _ => return None,
        };

        if content.is_empty() {
            None
        } else {
            Some(content)
        }
    }

    /// Parse a Nextcloud Talk webhook payload into channel messages.
    ///
    /// Relevant payload fields:
    /// - `type` (expects `message`)
    /// - `object.token` (room token for reply routing)
    /// - `message.actorType`, `message.actorId`, `message.message`, `message.timestamp`
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        let event_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let is_legacy_message_event = event_type.eq_ignore_ascii_case("message");
        let is_activity_streams_event = event_type.eq_ignore_ascii_case("create");

        if !is_legacy_message_event && !is_activity_streams_event {
            tracing::debug!("Nextcloud Talk: skipping non-message event: {event_type}");
            return messages;
        }

        if is_activity_streams_event {
            let object_type = payload
                .get("object")
                .and_then(|obj| obj.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !object_type.eq_ignore_ascii_case("note") {
                tracing::debug!(
                    "Nextcloud Talk: skipping Activity Streams event with unsupported object.type: {object_type}"
                );
                return messages;
            }
        }

        let message_obj = payload.get("message");

        let room_token = payload
            .get("object")
            .and_then(|obj| obj.get("token"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                message_obj
                    .and_then(|msg| msg.get("token"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                payload
                    .get("target")
                    .and_then(|target| target.get("id"))
                    .and_then(|v| v.as_str())
            })
            .map(str::trim)
            .filter(|token| !token.is_empty());

        let Some(room_token) = room_token else {
            tracing::warn!("Nextcloud Talk: missing room token in webhook payload");
            return messages;
        };

        let actor_type = message_obj
            .and_then(|msg| msg.get("actorType"))
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("actorType").and_then(|v| v.as_str()))
            .or_else(|| {
                payload
                    .get("actor")
                    .and_then(|actor| actor.get("type"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");

        // Ignore bot-originated messages to prevent feedback loops.
        if actor_type.eq_ignore_ascii_case("bots") || actor_type.eq_ignore_ascii_case("application")
        {
            tracing::debug!("Nextcloud Talk: skipping bot-originated message");
            return messages;
        }

        let actor_id = message_obj
            .and_then(|msg| msg.get("actorId"))
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("actorId").and_then(|v| v.as_str()))
            .or_else(|| {
                payload
                    .get("actor")
                    .and_then(|actor| actor.get("id"))
                    .and_then(|v| v.as_str())
            })
            .map(str::trim)
            .filter(|id| !id.is_empty());

        let Some(actor_id) = actor_id else {
            tracing::warn!("Nextcloud Talk: missing actorId in webhook payload");
            return messages;
        };
        let sender_id = Self::canonical_actor_id(actor_id);

        if !self.is_user_allowed(actor_id) {
            tracing::warn!(
                "Nextcloud Talk: ignoring message from unauthorized actor: {actor_id}. \
                Add to channels.nextcloud_talk.allowed_users in config.toml, \
                or run `zeroclaw onboard --channels-only` to configure interactively."
            );
            return messages;
        }

        if is_legacy_message_event {
            let message_type = message_obj
                .and_then(|msg| msg.get("messageType"))
                .and_then(|v| v.as_str())
                .unwrap_or("comment");
            if !message_type.eq_ignore_ascii_case("comment") {
                tracing::debug!("Nextcloud Talk: skipping non-comment messageType: {message_type}");
                return messages;
            }
        }

        // Ignore pure system messages.
        if is_legacy_message_event {
            let has_system_message = message_obj
                .and_then(|msg| msg.get("systemMessage"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            if has_system_message {
                tracing::debug!("Nextcloud Talk: skipping system message event");
                return messages;
            }
        }

        let content = message_obj
            .and_then(|msg| msg.get("message"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| Self::extract_content_from_as2_object(payload));

        let Some(content) = content else {
            return messages;
        };

        let message_id = Self::value_to_string(message_obj.and_then(|msg| msg.get("id")))
            .or_else(|| Self::value_to_string(payload.get("object").and_then(|obj| obj.get("id"))))
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let timestamp = Self::parse_timestamp_secs(
            message_obj
                .and_then(|msg| msg.get("timestamp"))
                .or_else(|| payload.get("timestamp")),
        );

        messages.push(ChannelMessage {
            id: message_id,
            reply_target: room_token.to_string(),
            sender: sender_id.to_string(),
            content,
            channel: "nextcloud_talk".to_string(),
            timestamp,
            thread_ts: None,
        });

        messages
    }

    async fn send_to_room(&self, room_token: &str, content: &str) -> anyhow::Result<()> {
        let encoded_room = urlencoding::encode(room_token);
        let url = format!(
            "{}/ocs/v2.php/apps/spreed/api/v1/chat/{}?format=json",
            self.base_url, encoded_room
        );

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.app_token)
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .json(&serde_json::json!({ "message": content }))
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let sanitized = crate::providers::sanitize_api_error(&body);
        tracing::error!("Nextcloud Talk send failed: {status} — {sanitized}");
        anyhow::bail!("Nextcloud Talk API error: {status}");
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
    use ring::hmac;

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
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    hmac::verify(&key, payload.as_bytes(), &provided).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    fn make_channel() -> NextcloudTalkChannel {
        NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
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
            "app-token".into(),
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
            "app-token".into(),
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
    fn nextcloud_talk_parse_activity_streams_create_note_payload() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            vec!["test".into()],
        );

        let payload = serde_json::json!({
            "type": "Create",
            "actor": {
                "type": "Person",
                "id": "users/test",
                "name": "test"
            },
            "object": {
                "type": "Note",
                "id": "177",
                "content": "{\"message\":\"hello\",\"parameters\":[]}",
                "mediaType": "text/markdown"
            },
            "target": {
                "type": "Collection",
                "id": "yyrubgfp",
                "name": "TESTCHAT"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "177");
        assert_eq!(messages[0].reply_target, "yyrubgfp");
        assert_eq!(messages[0].sender, "test");
        assert_eq!(messages[0].content, "hello");
    }

    #[test]
    fn nextcloud_talk_parse_activity_streams_skips_application_actor() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            vec!["*".into()],
        );

        let payload = serde_json::json!({
            "type": "Create",
            "actor": {
                "type": "Application",
                "id": "apps/zeroclaw"
            },
            "object": {
                "type": "Note",
                "id": "178",
                "content": "{\"message\":\"ignore me\"}"
            },
            "target": {
                "id": "yyrubgfp"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(messages.is_empty());
    }

    #[test]
    fn nextcloud_talk_allowlist_matches_full_and_short_actor_ids() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            vec!["users/test".into()],
        );
        assert!(channel.is_user_allowed("users/test"));
        assert!(channel.is_user_allowed("test"));
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
            "app-token".into(),
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
            "app-token".into(),
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
