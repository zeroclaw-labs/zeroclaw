use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// Nextcloud Talk channel in webhook mode.
///
/// Incoming messages are received by the gateway endpoint `/nextcloud-talk`.
/// Outbound replies are sent through Nextcloud Talk OCS API.
pub struct NextcloudTalkChannel {
    base_url: String,
    app_token: String,
    bot_name: String,
    allowed_users: Vec<String>,
    client: reqwest::Client,
}

impl NextcloudTalkChannel {
    pub fn new(
        base_url: String,
        app_token: String,
        bot_name: String,
        allowed_users: Vec<String>,
    ) -> Self {
        Self::new_with_proxy(base_url, app_token, bot_name, allowed_users, None)
    }

    pub fn new_with_proxy(
        base_url: String,
        app_token: String,
        bot_name: String,
        allowed_users: Vec<String>,
        proxy_url: Option<String>,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            app_token,
            bot_name: bot_name.to_ascii_lowercase(),
            allowed_users,
            client: zeroclaw_config::schema::build_channel_proxy_client(
                "channel.nextcloud_talk",
                proxy_url.as_deref(),
            ),
        }
    }

    fn is_user_allowed(&self, actor_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == actor_id)
    }

    /// Returns true if the given name/id belongs to this bot itself.
    ///
    /// Prevents feedback loops where ZeroClaw reacts to its own messages.
    fn is_bot_name(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        // Match the configured bot name, or the known bot name "zeroclaw".
        (!self.bot_name.is_empty() && name == self.bot_name) || name == "zeroclaw"
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
    /// Two payload formats are supported:
    ///
    /// **Format A — legacy/custom** (`type: "message"`):
    /// ```json
    /// {
    ///   "type": "message",
    ///   "object": { "token": "<room>" },
    ///   "message": { "actorId": "...", "message": "...", ... }
    /// }
    /// ```
    ///
    /// **Format B — Activity Streams 2.0** (`type: "Create"`):
    /// This is the format actually sent by Nextcloud Talk bot webhooks.
    /// ```json
    /// {
    ///   "type": "Create",
    ///   "actor": { "type": "Person", "id": "users/alice", "name": "Alice" },
    ///   "object": { "type": "Note", "id": "177", "content": "{\"message\":\"hi\",\"parameters\":[]}", "mediaType": "text/markdown" },
    ///   "target": { "type": "Collection", "id": "<room_token>", "name": "Room Name" }
    /// }
    /// ```
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let messages = Vec::new();

        let event_type = match payload.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return messages,
        };

        // Activity Streams 2.0 format sent by Nextcloud Talk bot webhooks.
        if event_type.eq_ignore_ascii_case("create") {
            return self.parse_as2_payload(payload);
        }

        // Legacy/custom format.
        if !event_type.eq_ignore_ascii_case("message") {
            tracing::debug!("Nextcloud Talk: skipping non-message event: {event_type}");
            return messages;
        }

        self.parse_message_payload(payload)
    }

    /// Parse Activity Streams 2.0 `Create` payload (real Nextcloud Talk bot webhook format).
    fn parse_as2_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        let obj = match payload.get("object") {
            Some(o) => o,
            None => return messages,
        };

        // Only handle Note objects (= chat messages). Ignore reactions, etc.
        let object_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !object_type.eq_ignore_ascii_case("note") {
            tracing::debug!("Nextcloud Talk: skipping AS2 Create with object.type={object_type}");
            return messages;
        }

        // Room token is in target.id.
        let room_token = payload
            .get("target")
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty());

        let Some(room_token) = room_token else {
            tracing::warn!("Nextcloud Talk: missing target.id (room token) in AS2 payload");
            return messages;
        };

        // Actor — skip bot-originated messages to prevent feedback loops.
        let actor = payload.get("actor").cloned().unwrap_or_default();
        let actor_type = actor.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if actor_type.eq_ignore_ascii_case("application") {
            tracing::debug!(
                "Nextcloud Talk: skipping bot-originated AS2 message (type=Application)"
            );
            return messages;
        }

        // actor.id is "users/<id>" or "bots/<id>" — strip the prefix.
        let actor_id = actor
            .get("id")
            .and_then(|v| v.as_str())
            .map(|id| {
                id.trim_start_matches("users/")
                    .trim_start_matches("bots/")
                    .trim()
            })
            .filter(|id| !id.is_empty());

        let Some(actor_id) = actor_id else {
            tracing::warn!("Nextcloud Talk: missing actor.id in AS2 payload");
            return messages;
        };

        // Also skip by actor.id prefix or known bot names — Nextcloud does not always
        // set actor.type="Application" reliably for bot-sent messages.
        let raw_actor_id = actor.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if raw_actor_id.starts_with("bots/") {
            tracing::debug!(
                "Nextcloud Talk: skipping bot-originated AS2 message (id prefix=bots/)"
            );
            return messages;
        }
        let actor_name = actor
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.is_bot_name(&actor_name) {
            tracing::debug!(
                "Nextcloud Talk: skipping bot-originated AS2 message (name={actor_name})"
            );
            return messages;
        }

        if !self.is_user_allowed(actor_id) {
            tracing::warn!(
                "Nextcloud Talk: ignoring message from unauthorized actor: {actor_id}. \
                Add to channels.nextcloud_talk.allowed_users in config.toml, \
                or run `zeroclaw onboard --channels-only` to configure interactively."
            );
            return messages;
        }

        // Message text is JSON-encoded inside object.content.
        // e.g. content = "{\"message\":\"hello\",\"parameters\":[]}"
        let content = obj
            .get("content")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::trim)
                    .map(str::to_string)
            })
            .filter(|s| !s.is_empty());

        let Some(content) = content else {
            tracing::debug!("Nextcloud Talk: empty or unparseable AS2 message content");
            return messages;
        };

        let message_id =
            Self::value_to_string(obj.get("id")).unwrap_or_else(|| Uuid::new_v4().to_string());

        messages.push(ChannelMessage {
            id: message_id,
            reply_target: room_token.to_string(),
            sender: actor_id.to_string(),
            content,
            channel: "nextcloud_talk".to_string(),
            timestamp: Self::now_unix_secs(),
            thread_ts: None,
            interruption_scope_id: None,
            metadata: serde_json::Value::Null,
            attachments: vec![],
        });

        messages
    }

    /// Parse legacy `type: "message"` payload format.
    fn parse_message_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
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
            tracing::warn!("Nextcloud Talk: missing room token in webhook payload");
            return messages;
        };

        let actor_type = message_obj
            .get("actorType")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("actorType").and_then(|v| v.as_str()))
            .unwrap_or("");

        // Ignore bot-originated messages to prevent feedback loops.
        // Nextcloud Talk uses "bots" or "application" depending on version/context.
        if actor_type.eq_ignore_ascii_case("bots") || actor_type.eq_ignore_ascii_case("application")
        {
            tracing::debug!(
                "Nextcloud Talk: skipping bot-originated message (actorType={actor_type})"
            );
            return messages;
        }

        let actor_id = message_obj
            .get("actorId")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("actorId").and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|id| !id.is_empty());

        let Some(actor_id) = actor_id else {
            tracing::warn!("Nextcloud Talk: missing actorId in webhook payload");
            return messages;
        };

        // Also skip by known bot names in case actorType is not set reliably.
        if self.is_bot_name(actor_id) {
            tracing::debug!("Nextcloud Talk: skipping bot-originated message (actorId={actor_id})");
            return messages;
        }

        if !self.is_user_allowed(actor_id) {
            tracing::warn!(
                "Nextcloud Talk: ignoring message from unauthorized actor: {actor_id}. \
                Add to channels.nextcloud_talk.allowed_users in config.toml, \
                or run `zeroclaw onboard --channels-only` to configure interactively."
            );
            return messages;
        }

        let message_type = message_obj
            .get("messageType")
            .and_then(|v| v.as_str())
            .unwrap_or("comment");
        if !message_type.eq_ignore_ascii_case("comment") {
            tracing::debug!("Nextcloud Talk: skipping non-comment messageType: {message_type}");
            return messages;
        }

        // Ignore pure system messages.
        let has_system_message = message_obj
            .get("systemMessage")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        if has_system_message {
            tracing::debug!("Nextcloud Talk: skipping system message event");
            return messages;
        }

        let content = message_obj
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|content| !content.is_empty());

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
            interruption_scope_id: None,
            metadata: serde_json::Value::Null,
            attachments: vec![],
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
        tracing::error!("Nextcloud Talk send failed: {status} — {body}");
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
            "app-token".into(),
            "zeroclaw".into(),
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
            "zeroclaw".into(),
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
    fn nextcloud_talk_parse_as2_create_payload() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            vec!["*".into()],
        );
        // Real payload format sent by Nextcloud Talk bot webhooks.
        let payload = serde_json::json!({
            "type": "Create",
            "actor": {
                "type": "Person",
                "id": "users/user_a",
                "name": "User A",
                "talkParticipantType": "1"
            },
            "object": {
                "type": "Note",
                "id": "177",
                "name": "message",
                "content": "{\"message\":\"hallo, bist du da?\",\"parameters\":[]}",
                "mediaType": "text/markdown"
            },
            "target": {
                "type": "Collection",
                "id": "room-token-123",
                "name": "HOME"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].reply_target, "room-token-123");
        assert_eq!(messages[0].sender, "user_a");
        assert_eq!(messages[0].content, "hallo, bist du da?");
        assert_eq!(messages[0].channel, "nextcloud_talk");
    }

    #[test]
    fn nextcloud_talk_parse_as2_skips_bot_originated() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            vec!["*".into()],
        );
        let payload = serde_json::json!({
            "type": "Create",
            "actor": {
                "type": "Application",
                "id": "bots/zeroclaw",
                "name": "zeroclaw"
            },
            "object": {
                "type": "Note",
                "id": "178",
                "content": "{\"message\":\"I am the bot\",\"parameters\":[]}",
                "mediaType": "text/markdown"
            },
            "target": {
                "type": "Collection",
                "id": "room-token-123",
                "name": "HOME"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(messages.is_empty());
    }

    #[test]
    fn nextcloud_talk_parse_as2_skips_bot_by_name() {
        // Even if actor.type is not "Application", messages from the configured bot
        // name must be dropped to prevent feedback loops.
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            vec!["*".into()],
        );
        let payload = serde_json::json!({
            "type": "Create",
            "actor": {
                "type": "Person",        // <- wrong type, but name matches
                "id": "users/zeroclaw",
                "name": "zeroclaw"
            },
            "object": {
                "type": "Note",
                "id": "999",
                "content": "{\"message\":\"I am the bot\",\"parameters\":[]}",
                "mediaType": "text/markdown"
            },
            "target": {
                "type": "Collection",
                "id": "room-token-123",
                "name": "HOME"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(
            messages.is_empty(),
            "bot message should be filtered even if actor.type is wrong"
        );
    }

    #[test]
    fn nextcloud_talk_parse_message_skips_application_actor_type() {
        // parse_message_payload (legacy format) should also drop actorType=application.
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            vec!["*".into()],
        );
        let payload = serde_json::json!({
            "type": "message",
            "object": {"token": "room-token-123"},
            "message": {
                "actorType": "application",
                "actorId": "zeroclaw",
                "message": "Self message"
            }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(
            messages.is_empty(),
            "application actorType must be filtered in legacy format"
        );
    }

    #[test]
    fn nextcloud_talk_parse_as2_skips_non_note_objects() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            vec!["*".into()],
        );
        let payload = serde_json::json!({
            "type": "Create",
            "actor": { "type": "Person", "id": "users/user_a" },
            "object": { "type": "Reaction", "id": "5" },
            "target": { "type": "Collection", "id": "room-token-123" }
        });

        let messages = channel.parse_webhook_payload(&payload);
        assert!(messages.is_empty());
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
            "zeroclaw".into(),
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
            "app-token".into(),
            "zeroclaw".into(),
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
            "zeroclaw".into(),
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
