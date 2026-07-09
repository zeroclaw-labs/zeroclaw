use async_trait::async_trait;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use reqwest::Method;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::StreamMode;

/// Maximum message length accepted by Nextcloud Talk (characters, not bytes).
/// The OCS API rejects messages longer than 32 000 characters.
const NC_MAX_MESSAGE_LENGTH: usize = 32_000;

/// Default minimum interval between draft edits when not configured explicitly.
const DEFAULT_DRAFT_UPDATE_INTERVAL_MS: u64 = 1000;

/// Nextcloud Talk channel in webhook mode.
/// Incoming messages are received by the gateway endpoint `/nextcloud-talk`.
/// Outbound replies are sent through Nextcloud Talk OCS API.
pub struct NextcloudTalkChannel {
    base_url: String,
    app_token: String,
    bot_name: String,
    /// The alias key under `[channels.nextcloud_talk.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    client: reqwest::Client,
    /// Controls whether and how streaming draft updates are delivered.
    stream_mode: StreamMode,
    /// Minimum interval (ms) between mid-stream draft edits per room.
    draft_update_interval_ms: u64,
    /// Tracks the last time a draft-edit was sent per room token, for rate-limiting.
    last_draft_edit: Mutex<HashMap<String, std::time::Instant>>,
}

impl NextcloudTalkChannel {
    pub fn new(
        base_url: String,
        app_token: String,
        bot_name: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self::new_with_proxy(base_url, app_token, bot_name, alias, peer_resolver, None)
    }

    pub fn new_with_proxy(
        base_url: String,
        app_token: String,
        bot_name: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        proxy_url: Option<String>,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            app_token,
            bot_name: bot_name.to_ascii_lowercase(),
            alias: alias.into(),
            peer_resolver,
            client: zeroclaw_config::schema::build_channel_proxy_client(
                "channel.nextcloud_talk",
                proxy_url.as_deref(),
            ),
            stream_mode: StreamMode::Off,
            draft_update_interval_ms: DEFAULT_DRAFT_UPDATE_INTERVAL_MS,
            last_draft_edit: Mutex::new(HashMap::new()),
        }
    }

    /// Return the alias under `[channels.nextcloud_talk.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Configure streaming draft-update behaviour.
    /// `mode` — `Off` disables draft updates entirely; `Partial` enables live edits.
    /// `interval_ms` — minimum delay between consecutive OCS edit calls per room.
    pub fn with_streaming(mut self, mode: StreamMode, interval_ms: u64) -> Self {
        self.stream_mode = mode;
        self.draft_update_interval_ms = interval_ms;
        self
    }

    fn is_user_allowed(&self, actor_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, actor_id, crate::allowlist::Match::Sensitive)
    }

    /// Returns true if the given name/id belongs to this bot itself.
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
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"event_type": event_type})),
                "Talk: skipping non-message event"
            );
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
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"object_type": object_type})),
                "Talk: skipping AS2 Create with object.type="
            );
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Talk: missing target.id (room token) in AS2 payload"
            );
            return messages;
        };

        // Actor — skip bot-originated messages to prevent feedback loops.
        let actor = payload.get("actor").cloned().unwrap_or_default();
        let actor_type = actor.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if actor_type.eq_ignore_ascii_case("application") {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Talk: skipping bot-originated AS2 message (type=Application)"
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Talk: missing actor.id in AS2 payload"
            );
            return messages;
        };

        // Also skip by actor.id prefix or known bot names — Nextcloud does not always
        // set actor.type="Application" reliably for bot-sent messages.
        let raw_actor_id = actor.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if raw_actor_id.starts_with("bots/") {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Talk: skipping bot-originated AS2 message (id prefix=bots/)"
            );
            return messages;
        }
        let actor_name = actor
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.is_bot_name(&actor_name) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"actor_name": actor_name})),
                "Talk: skipping bot-originated AS2 message (name=)"
            );
            return messages;
        }

        if !self.is_user_allowed(actor_id) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"actor_id": actor_id})),
                "Talk: ignoring message from unauthorized actor: . Add to channels.nextcloud_talk.allowed_users in config.toml, or run `zeroclaw config set channels.nextcloud_talk.allowed-users='[\"<user-id>\"]'`."
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
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Talk: empty or unparseable AS2 message content"
            );
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
            channel_alias: Some(self.alias.clone()),
            timestamp: Self::now_unix_secs(),
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Talk: missing room token in webhook payload"
            );
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
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"actor_type": actor_type})),
                "Talk: skipping bot-originated message (actorType=)"
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Talk: missing actorId in webhook payload"
            );
            return messages;
        };

        // Also skip by known bot names in case actorType is not set reliably.
        if self.is_bot_name(actor_id) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"actor_id": actor_id})),
                "Talk: skipping bot-originated message (actorId=)"
            );
            return messages;
        }

        if !self.is_user_allowed(actor_id) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"actor_id": actor_id})),
                "Talk: ignoring message from unauthorized actor: . Add to channels.nextcloud_talk.allowed_users in config.toml, or run `zeroclaw config set channels.nextcloud_talk.allowed-users='[\"<user-id>\"]'`."
            );
            return messages;
        }

        let message_type = message_obj
            .get("messageType")
            .and_then(|v| v.as_str())
            .unwrap_or("comment");
        if !message_type.eq_ignore_ascii_case("comment") {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"message_type": message_type})),
                "Talk: skipping non-comment messageType"
            );
            return messages;
        }

        // Ignore pure system messages.
        let has_system_message = message_obj
            .get("systemMessage")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        if has_system_message {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Talk: skipping system message event"
            );
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
            channel_alias: Some(self.alias.clone()),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        });

        messages
    }

    fn ocs_chat_url(&self, room_token: &str, message_id: Option<&str>) -> String {
        let encoded_room = urlencoding::encode(room_token);
        match message_id {
            Some(message_id) => format!(
                "{}/ocs/v2.php/apps/spreed/api/v1/chat/{}/{}?format=json",
                self.base_url, encoded_room, message_id
            ),
            None => format!(
                "{}/ocs/v2.php/apps/spreed/api/v1/chat/{}?format=json",
                self.base_url, encoded_room
            ),
        }
    }

    fn ocs_chat_request(
        &self,
        method: Method,
        room_token: &str,
        message_id: Option<&str>,
    ) -> reqwest::RequestBuilder {
        self.client
            .request(method, self.ocs_chat_url(room_token, message_id))
            .bearer_auth(&self.app_token)
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
    }

    async fn post_to_room(
        &self,
        room_token: &str,
        content: &str,
    ) -> anyhow::Result<reqwest::Response> {
        Ok(self
            .ocs_chat_request(Method::POST, room_token, None)
            .json(&serde_json::json!({ "message": content }))
            .send()
            .await?)
    }

    async fn send_to_room(&self, room_token: &str, content: &str) -> anyhow::Result<()> {
        let response = self.post_to_room(room_token, content).await?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"status": status.to_string(), "body": body})),
            "Talk send failed:"
        );
        anyhow::bail!("Talk API error: {status}");
    }

    /// Send a message and return the numeric message ID assigned by Nextcloud Talk.
    async fn send_to_room_with_id(
        &self,
        room_token: &str,
        content: &str,
    ) -> anyhow::Result<String> {
        let response = self.post_to_room(room_token, content).await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"status": status.to_string(), "body": body})),
                "Talk send_to_room_with_id failed:"
            );
            anyhow::bail!("Talk API error: {status}");
        }

        // Response: { "ocs": { "data": { "id": 42, ... } } }
        let body: serde_json::Value = response.json().await?;
        let message_id = body
            .pointer("/ocs/data/id")
            .and_then(|v| v.as_u64())
            .map(|id| id.to_string())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "Talk: missing message ID in send response"
                );
                anyhow::Error::msg("Talk: missing message ID in send response")
            })?;

        Ok(message_id)
    }

    /// Edit an existing message via the Nextcloud Talk OCS API.
    /// `PUT /ocs/v2.php/apps/spreed/api/v1/chat/{token}/{messageId}`
    async fn edit_message(
        &self,
        room_token: &str,
        message_id: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        let response = self
            .ocs_chat_request(Method::PUT, room_token, Some(message_id))
            .json(&serde_json::json!({ "message": content }))
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Talk edit API error: {status}: {body}");
    }

    /// Delete a message via the Nextcloud Talk OCS API.
    /// `DELETE /ocs/v2.php/apps/spreed/api/v1/chat/{token}/{messageId}`
    async fn delete_message(&self, room_token: &str, message_id: &str) -> anyhow::Result<()> {
        let response = self
            .ocs_chat_request(Method::DELETE, room_token, Some(message_id))
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"status": status.to_string(), "body": body})),
            "Talk delete_message failed"
        );
        anyhow::bail!("Talk delete API error: {status}");
    }

    /// Truncate text to the Nextcloud Talk character limit (UTF-8 char boundary safe).
    fn truncate_to_nc_limit(text: &str) -> &str {
        match text.char_indices().nth(NC_MAX_MESSAGE_LENGTH) {
            Some((end, _)) => &text[..end],
            None => text,
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for NextcloudTalkChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::NextcloudTalk,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
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

    fn supports_draft_updates(&self) -> bool {
        self.stream_mode != StreamMode::Off
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if self.stream_mode == StreamMode::Off {
            return Ok(None);
        }

        // Send a placeholder "..." message and track its ID for later edits.
        let initial = if message.content.is_empty() {
            "..."
        } else {
            &message.content
        };
        let initial = Self::truncate_to_nc_limit(initial);
        match self.send_to_room_with_id(&message.recipient, initial).await {
            Ok(id) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(
                            ::serde_json::json!({"room": message.recipient, "message_id": id})
                        ),
                    "Talk: draft message sent"
                );
                self.last_draft_edit
                    .lock()
                    .insert(message.recipient.clone(), std::time::Instant::now());
                Ok(Some(id))
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "Talk: send_draft failed, falling back to final send"
                );
                Err(e)
            }
        }
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        // Rate-limit mid-stream edits per room to avoid hammering the API.
        {
            let last_edits = self.last_draft_edit.lock();
            if let Some(last_time) = last_edits.get(recipient) {
                let elapsed = u64::try_from(last_time.elapsed().as_millis()).unwrap_or(u64::MAX);
                if elapsed < self.draft_update_interval_ms {
                    return Ok(());
                }
            }
        }

        let display_text = Self::truncate_to_nc_limit(text);

        match self.edit_message(recipient, message_id, display_text).await {
            Ok(()) => {
                self.last_draft_edit
                    .lock()
                    .insert(recipient.to_string(), std::time::Instant::now());
            }
            Err(e) => {
                // Non-fatal: log and continue. The final send will still deliver the
                // complete response even if mid-stream edits fail.
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Talk update_draft edit failed; final response will still be delivered"
                );
            }
        }

        Ok(())
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        _suppress_voice: bool,
    ) -> anyhow::Result<()> {
        let display_text = Self::truncate_to_nc_limit(text);

        match self.edit_message(recipient, message_id, display_text).await {
            Ok(()) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(
                            ::serde_json::json!({"room": recipient, "message_id": message_id})
                        ),
                    "Talk: draft finalized"
                );
                Ok(())
            }
            Err(e) => {
                // Edit failed (e.g. message too old, permissions) — delete and re-send.
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "Talk finalize_draft edit failed ; attempting delete+resend"
                );
                let _ = self.delete_message(recipient, message_id).await;
                self.send_to_room(recipient, display_text).await
            }
        }
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        if let Err(e) = self.delete_message(recipient, message_id).await {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Talk cancel_draft delete failed (non-fatal)"
            );
        }
        self.last_draft_edit.lock().remove(recipient);
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Talk channel active (webhook mode). \
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

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // Typing endpoint not wired; no-op stub keeps the trait surface uniform.
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Verify Nextcloud Talk webhook signature.
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
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Talk: missing X-Nextcloud-Talk-Random header"
        );
        return false;
    }

    let signature_hex = signature
        .trim()
        .strip_prefix("sha256=")
        .unwrap_or(signature)
        .trim();

    let Ok(provided) = hex::decode(signature_hex) else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Talk: invalid signature format"
        );
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
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn nextcloud_talk_channel_name() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        );
        assert_eq!(channel.name(), "nextcloud_talk");
    }

    #[test]
    fn supports_draft_updates_off_by_default() {
        // Default construction uses StreamMode::Off → draft updates disabled.
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        );
        assert!(!channel.supports_draft_updates());
    }

    #[test]
    fn supports_draft_updates_true_when_partial() {
        use zeroclaw_config::schema::StreamMode;
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        )
        .with_streaming(StreamMode::Partial, 800);
        assert!(channel.supports_draft_updates());
    }

    #[test]
    fn truncate_to_nc_limit_short_text_unchanged() {
        let text = "hello";
        assert_eq!(NextcloudTalkChannel::truncate_to_nc_limit(text), text);
    }

    #[test]
    fn truncate_to_nc_limit_exact_limit_unchanged() {
        let text = "a".repeat(NC_MAX_MESSAGE_LENGTH);
        let result = NextcloudTalkChannel::truncate_to_nc_limit(&text);
        assert_eq!(result.len(), NC_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn truncate_to_nc_limit_over_limit_is_truncated() {
        let text = "a".repeat(NC_MAX_MESSAGE_LENGTH + 100);
        let result = NextcloudTalkChannel::truncate_to_nc_limit(&text);
        assert_eq!(result.chars().count(), NC_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn truncate_to_nc_limit_multibyte_safe() {
        // Each emoji is 4 bytes but 1 char — must not split in the middle.
        let text = "🦀".repeat(NC_MAX_MESSAGE_LENGTH + 10);
        let result = NextcloudTalkChannel::truncate_to_nc_limit(&text);
        assert_eq!(result.chars().count(), NC_MAX_MESSAGE_LENGTH);
        // Must be valid UTF-8.
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn update_draft_rate_limit_short_circuits_network() {
        use zeroclaw_config::schema::StreamMode;
        // Use a large interval (60 s) so the rate-limit always fires immediately.
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        )
        .with_streaming(StreamMode::Partial, 60_000);
        channel
            .last_draft_edit
            .lock()
            .insert("room-token-123".to_string(), std::time::Instant::now());

        // update_draft should return Ok immediately without hitting the network.
        let result = channel
            .update_draft("room-token-123", "42", "some delta")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_draft_returns_none_when_stream_mode_off() {
        use zeroclaw_api::channel::SendMessage;
        // Default mode is Off — send_draft must short-circuit.
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        );
        let result = channel
            .send_draft(&SendMessage::new("...", "room-token-123"))
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn send_succeeds_without_message_id_in_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ocs/v2.php/apps/spreed/api/v1/chat/room-token-123"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ocs": { "data": {} }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let channel = NextcloudTalkChannel::new(
            server.uri(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        );
        let result = channel
            .send(&SendMessage::new("hello", "room-token-123"))
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_draft_returns_message_id_from_response() {
        use zeroclaw_config::schema::StreamMode;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ocs/v2.php/apps/spreed/api/v1/chat/room-token-123"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ocs": { "data": { "id": 42 } }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let channel = NextcloudTalkChannel::new(
            server.uri(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        )
        .with_streaming(StreamMode::Partial, 1000);
        let result = channel
            .send_draft(&SendMessage::new("hello", "room-token-123"))
            .await;

        assert_eq!(result.unwrap(), Some("42".to_string()));
    }

    #[tokio::test]
    async fn send_draft_errors_when_message_id_missing() {
        use zeroclaw_config::schema::StreamMode;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ocs/v2.php/apps/spreed/api/v1/chat/room-token-123"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ocs": { "data": {} }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let channel = NextcloudTalkChannel::new(
            server.uri(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        )
        .with_streaming(StreamMode::Partial, 1000);
        let result = channel
            .send_draft(&SendMessage::new("hello", "room-token-123"))
            .await;

        assert!(result.is_err());
    }

    #[test]
    fn nextcloud_talk_user_allowlist_exact_and_wildcard() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        );
        assert!(channel.is_user_allowed("user_a"));
        assert!(!channel.is_user_allowed("user_b"));

        let wildcard = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert!(wildcard.is_user_allowed("any_user"));
    }

    #[test]
    fn nextcloud_talk_parse_valid_message_payload() {
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        );
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
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
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
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
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
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
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
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
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
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
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
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        );
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
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
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
        let channel = NextcloudTalkChannel::new(
            "https://cloud.example.com".into(),
            "app-token".into(),
            "zeroclaw".into(),
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["user_a".into()]),
        );
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
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
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
            "nextcloud_talk_test_alias",
            Arc::new(|| vec!["*".into()]),
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
