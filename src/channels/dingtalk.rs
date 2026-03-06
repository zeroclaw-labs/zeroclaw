use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

const DINGTALK_BOT_CALLBACK_TOPIC: &str = "/v1.0/im/bot/messages/get";

/// DingTalk channel — connects via Stream Mode WebSocket for real-time messages.
/// Replies are sent through per-message session webhook URLs.
pub struct DingTalkChannel {
    client_id: String,
    client_secret: String,
    allowed_users: Vec<String>,
    message_type: crate::config::schema::DingTalkMessageType,
    card_template_id: Option<String>,
    robot_code: Option<String>,
    card_template_key: String,
    /// Per-chat session webhooks for sending replies (chatID -> webhook URL).
    /// DingTalk provides a unique webhook URL with each incoming message.
    session_webhooks: Arc<RwLock<HashMap<String, String>>>,
    access_token_cache: Arc<RwLock<Option<CachedAccessToken>>>,
    draft_states: Arc<RwLock<HashMap<String, CardDraftState>>>,
}

/// Response from DingTalk gateway connection registration.
#[derive(serde::Deserialize)]
struct GatewayResponse {
    endpoint: String,
    ticket: String,
}

#[derive(Clone)]
struct CachedAccessToken {
    value: String,
    refresh_after: Instant,
}

#[derive(Clone, Default)]
struct CardDraftState {
    recipient: String,
    last_content: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccessTokenResponse {
    access_token: String,
    expire_in: u64,
}

impl DingTalkChannel {
    pub fn new(
        client_id: String,
        client_secret: String,
        allowed_users: Vec<String>,
        message_type: crate::config::schema::DingTalkMessageType,
        card_template_id: Option<String>,
        robot_code: Option<String>,
        card_template_key: String,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            allowed_users,
            message_type,
            card_template_id,
            robot_code,
            card_template_key,
            session_webhooks: Arc::new(RwLock::new(HashMap::new())),
            access_token_cache: Arc::new(RwLock::new(None)),
            draft_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client_with_timeouts("channel.dingtalk", 120, 10)
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    fn card_template_key(&self) -> &str {
        // Keep runtime behavior consistent with config defaults even if value is blank.
        if self.card_template_key.trim().is_empty() {
            "content"
        } else {
            self.card_template_key.as_str()
        }
    }

    fn card_streaming_enabled(&self) -> bool {
        if self.message_type != crate::config::schema::DingTalkMessageType::Card {
            return false;
        }
        self.card_template_id
            .as_ref()
            .is_some_and(|id| !id.trim().is_empty())
    }

    fn robot_code_or_client_id(&self) -> &str {
        // `robot_code` is optional; fall back to app key when not provided.
        self.robot_code
            .as_deref()
            .filter(|code| !code.trim().is_empty())
            .unwrap_or(self.client_id.as_str())
    }

    async fn invalidate_access_token(&self) {
        let mut cache = self.access_token_cache.write().await;
        *cache = None;
    }

    async fn get_access_token(&self) -> anyhow::Result<String> {
        let now = Instant::now();
        if let Some(cached) = self.access_token_cache.read().await.as_ref() {
            if cached.refresh_after > now {
                return Ok(cached.value.clone());
            }
        }

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/oauth2/accessToken")
            .json(&serde_json::json!({
                "appKey": self.client_id,
                "appSecret": self.client_secret,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("DingTalk access token failed ({status}): {sanitized}");
        }

        let payload: AccessTokenResponse = resp.json().await?;
        let ttl = payload.expire_in.max(120);
        let refresh_after = now + Duration::from_secs(ttl.saturating_sub(60));

        {
            let mut cache = self.access_token_cache.write().await;
            *cache = Some(CachedAccessToken {
                value: payload.access_token.clone(),
                refresh_after,
            });
        }

        Ok(payload.access_token)
    }

    async fn create_card_draft(&self, recipient: &str) -> anyhow::Result<String> {
        let template_id = self
            .card_template_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("card_template_id is required for card mode"))?;
        let draft_id = format!("card_{}", Uuid::new_v4());
        let is_group = recipient.starts_with("cid");
        let open_space_id = if is_group {
            format!("dtv1.card//IM_GROUP.{recipient}")
        } else {
            format!("dtv1.card//IM_ROBOT.{recipient}")
        };

        let mut body = serde_json::Map::new();
        body.insert(
            "cardTemplateId".to_string(),
            serde_json::Value::String(template_id.to_string()),
        );
        body.insert(
            "outTrackId".to_string(),
            serde_json::Value::String(draft_id.clone()),
        );
        body.insert(
            "cardData".to_string(),
            serde_json::json!({
                "cardParamMap": {
                    self.card_template_key(): ""
                }
            }),
        );
        body.insert(
            "callbackType".to_string(),
            serde_json::Value::String("STREAM".to_string()),
        );
        body.insert(
            "imGroupOpenSpaceModel".to_string(),
            serde_json::json!({ "supportForward": true }),
        );
        body.insert(
            "imRobotOpenSpaceModel".to_string(),
            serde_json::json!({ "supportForward": true }),
        );
        body.insert(
            "openSpaceId".to_string(),
            serde_json::Value::String(open_space_id),
        );
        body.insert(
            "userIdType".to_string(),
            serde_json::Value::Number(serde_json::Number::from(1_u64)),
        );
        if is_group {
            body.insert(
                "imGroupOpenDeliverModel".to_string(),
                serde_json::json!({
                    "robotCode": self.robot_code_or_client_id(),
                }),
            );
        } else {
            body.insert(
                "imRobotOpenDeliverModel".to_string(),
                serde_json::json!({
                    "spaceType": "IM_ROBOT",
                    "robotCode": self.robot_code_or_client_id(),
                }),
            );
        }

        let token = self.get_access_token().await?;
        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/card/instances/createAndDeliver")
            .header("x-acs-dingtalk-access-token", token)
            .header("Content-Type", "application/json")
            .json(&serde_json::Value::Object(body))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("DingTalk card createAndDeliver failed ({status}): {sanitized}");
        }

        Ok(draft_id)
    }

    async fn stream_card(&self, draft_id: &str, content: &str, finalize: bool) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "outTrackId": draft_id,
            "guid": Uuid::new_v4().to_string(),
            "key": self.card_template_key(),
            "content": content,
            "isFull": true,
            "isFinalize": finalize,
            "isError": false,
        });

        let token = self.get_access_token().await?;
        let resp = self
            .http_client()
            .put("https://api.dingtalk.com/v1.0/card/streaming")
            .header("x-acs-dingtalk-access-token", token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.invalidate_access_token().await;
            let refreshed = self.get_access_token().await?;
            let retry = self
                .http_client()
                .put("https://api.dingtalk.com/v1.0/card/streaming")
                .header("x-acs-dingtalk-access-token", refreshed)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await?;

            if retry.status().is_success() {
                return Ok(());
            }

            let status = retry.status();
            let err = retry.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("DingTalk card streaming retry failed ({status}): {sanitized}");
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("DingTalk card streaming failed ({status}): {sanitized}");
        }

        Ok(())
    }

    async fn lookup_session_webhook(&self, recipient: &str) -> anyhow::Result<String> {
        let webhooks = self.session_webhooks.read().await;
        webhooks.get(recipient).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "No session webhook found for chat {}. \
                 The user must send a message first to establish a session.",
                recipient
            )
        })
    }

    async fn send_markdown_via_session(&self, message: &SendMessage) -> anyhow::Result<()> {
        let webhook_url = self.lookup_session_webhook(&message.recipient).await?;

        let title = message.subject.as_deref().unwrap_or("ZeroClaw");
        let body = serde_json::json!({
            "msgtype": "markdown",
            "markdown": {
                "title": title,
                "text": message.content,
            }
        });

        let resp = self
            .http_client()
            .post(&webhook_url)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("DingTalk webhook reply failed ({status}): {sanitized}");
        }

        Ok(())
    }

    fn parse_stream_data(frame: &serde_json::Value) -> Option<serde_json::Value> {
        match frame.get("data") {
            Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
            Some(serde_json::Value::Object(_)) => frame.get("data").cloned(),
            _ => None,
        }
    }

    fn resolve_chat_id(data: &serde_json::Value, sender_id: &str) -> String {
        let is_private_chat = data
            .get("conversationType")
            .and_then(|value| {
                value
                    .as_str()
                    .map(|v| v == "1")
                    .or_else(|| value.as_i64().map(|v| v == 1))
            })
            .unwrap_or(true);

        if is_private_chat {
            sender_id.to_string()
        } else {
            data.get("conversationId")
                .and_then(|c| c.as_str())
                .unwrap_or(sender_id)
                .to_string()
        }
    }

    /// Register a connection with DingTalk's gateway to get a WebSocket endpoint.
    async fn register_connection(&self) -> anyhow::Result<GatewayResponse> {
        let body = serde_json::json!({
            "clientId": self.client_id,
            "clientSecret": self.client_secret,
            "subscriptions": [
                {
                    "type": "CALLBACK",
                    "topic": DINGTALK_BOT_CALLBACK_TOPIC,
                }
            ],
        });

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/gateway/connections/open")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("DingTalk gateway registration failed ({status}): {sanitized}");
        }

        let gw: GatewayResponse = resp.json().await?;
        Ok(gw)
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &str {
        "dingtalk"
    }

    fn supports_draft_updates(&self) -> bool {
        self.card_streaming_enabled()
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.send_markdown_via_session(message).await
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if !self.card_streaming_enabled() {
            return Ok(None);
        }

        let draft_id = match self.create_card_draft(&message.recipient).await {
            Ok(id) => id,
            Err(err) => {
                tracing::warn!("DingTalk: failed to create card draft, falling back to markdown: {err}");
                return Ok(None);
            }
        };

        {
            let mut drafts = self.draft_states.write().await;
            drafts.insert(
                draft_id.clone(),
                CardDraftState {
                    recipient: message.recipient.clone(),
                    last_content: String::new(),
                },
            );
        }

        Ok(Some(draft_id))
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<Option<String>> {
        if !self.card_streaming_enabled() {
            return Ok(None);
        }

        if text.trim().is_empty() {
            return Ok(None);
        }

        let known_recipient = {
            let drafts = self.draft_states.read().await;
            drafts.get(message_id).map(|state| state.recipient.clone())
        };
        match known_recipient {
            Some(known) if known == recipient => {}
            Some(known) => {
                tracing::debug!(
                    "DingTalk: skipping draft update due to recipient mismatch (expected {known}, got {recipient})"
                );
                return Ok(None);
            }
            None => {
                tracing::debug!("DingTalk: skipping draft update for unknown message_id: {message_id}");
                return Ok(None);
            }
        }

        self.stream_card(message_id, text, false).await?;

        {
            let mut drafts = self.draft_states.write().await;
            if let Some(state) = drafts.get_mut(message_id) {
                state.last_content = text.to_string();
            }
        }

        Ok(None)
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if !self.card_streaming_enabled() {
            return self
                .send(&SendMessage::new(text.to_string(), recipient.to_string()))
                .await;
        }

        let known_recipient = {
            let drafts = self.draft_states.read().await;
            drafts.get(message_id).map(|state| state.recipient.clone())
        };
        match known_recipient {
            Some(known) if known == recipient => {}
            Some(known) => {
                tracing::debug!(
                    "DingTalk: skipping draft finalize due to recipient mismatch (expected {known}, got {recipient})"
                );
                return Ok(());
            }
            None => {
                tracing::debug!(
                    "DingTalk: skipping draft finalize for unknown message_id: {message_id}"
                );
                return Ok(());
            }
        }

        let stream_result = self.stream_card(message_id, text, true).await;
        {
            let mut drafts = self.draft_states.write().await;
            drafts.remove(message_id);
        }

        if let Err(card_err) = stream_result {
            tracing::warn!(
                "DingTalk: card finalize failed, trying markdown session fallback: {card_err}"
            );
            self.send(&SendMessage::new(text.to_string(), recipient.to_string()))
                .await
                .map_err(|send_err| {
                    anyhow::anyhow!(
                        "DingTalk card finalize failed: {card_err}; markdown fallback failed: {send_err}"
                    )
                })?;
        }

        Ok(())
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        if !self.card_streaming_enabled() {
            return Ok(());
        }

        let known_recipient = {
            let drafts = self.draft_states.read().await;
            drafts.get(message_id).map(|state| state.recipient.clone())
        };
        match known_recipient {
            Some(known) if known == recipient => {}
            Some(known) => {
                tracing::debug!(
                    "DingTalk: skipping draft cancel due to recipient mismatch (expected {known}, got {recipient})"
                );
                return Ok(());
            }
            None => {
                tracing::debug!(
                    "DingTalk: skipping draft cancel for unknown message_id: {message_id}"
                );
                return Ok(());
            }
        }

        let fallback_content = {
            let mut drafts = self.draft_states.write().await;
            drafts
                .remove(message_id)
                .map(|state| {
                    if state.last_content.trim().is_empty() {
                        "Request cancelled.".to_string()
                    } else {
                        state.last_content
                    }
                })
                .unwrap_or_else(|| "Request cancelled.".to_string())
        };

        if let Err(err) = self.stream_card(message_id, &fallback_content, true).await {
            tracing::debug!("DingTalk: cancel_draft stream finalize failed: {err}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!("DingTalk: registering gateway connection...");

        let gw = self.register_connection().await?;
        let ws_url = format!("{}?ticket={}", gw.endpoint, gw.ticket);

        tracing::info!("DingTalk: connecting to stream WebSocket...");
        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        tracing::info!("DingTalk: connected and listening for messages...");

        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                    tracing::warn!("DingTalk WebSocket error: {sanitized}");
                    break;
                }
                _ => continue,
            };

            let frame: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let frame_type = frame.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match frame_type {
                "SYSTEM" => {
                    // Respond to system pings to keep the connection alive
                    let message_id = frame
                        .get("headers")
                        .and_then(|h| h.get("messageId"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("");

                    let pong = serde_json::json!({
                        "code": 200,
                        "headers": {
                            "contentType": "application/json",
                            "messageId": message_id,
                        },
                        "message": "OK",
                        "data": "",
                    });

                    if let Err(e) = write.send(Message::Text(pong.to_string().into())).await {
                        tracing::warn!("DingTalk: failed to send pong: {e}");
                        break;
                    }
                }
                "EVENT" | "CALLBACK" => {
                    // Parse the chatbot callback data from the frame.
                    let data = match Self::parse_stream_data(&frame) {
                        Some(v) => v,
                        None => {
                            tracing::debug!("DingTalk: frame has no parseable data payload");
                            continue;
                        }
                    };

                    // Extract message content
                    let content = data
                        .get("text")
                        .and_then(|t| t.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .trim();

                    if content.is_empty() {
                        continue;
                    }

                    let sender_id = data
                        .get("senderStaffId")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");

                    if !self.is_user_allowed(sender_id) {
                        tracing::warn!(
                            "DingTalk: ignoring message from unauthorized user: {sender_id}"
                        );
                        continue;
                    }

                    // Private chat uses sender ID, group chat uses conversation ID.
                    let chat_id = Self::resolve_chat_id(&data, sender_id);

                    // Store session webhook for later replies
                    if let Some(webhook) = data.get("sessionWebhook").and_then(|w| w.as_str()) {
                        let webhook = webhook.to_string();
                        let mut webhooks = self.session_webhooks.write().await;
                        // Use both keys so reply routing works for both group and private flows.
                        webhooks.insert(chat_id.clone(), webhook.clone());
                        webhooks.insert(sender_id.to_string(), webhook);
                    }

                    // Acknowledge the event
                    let message_id = frame
                        .get("headers")
                        .and_then(|h| h.get("messageId"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("");

                    let ack = serde_json::json!({
                        "code": 200,
                        "headers": {
                            "contentType": "application/json",
                            "messageId": message_id,
                        },
                        "message": "OK",
                        "data": "",
                    });
                    let _ = write.send(Message::Text(ack.to_string().into())).await;

                    let channel_msg = ChannelMessage {
                        id: Uuid::new_v4().to_string(),
                        sender: sender_id.to_string(),
                        reply_target: chat_id,
                        content: content.to_string(),
                        channel: "dingtalk".to_string(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        thread_ts: None,
                    };

                    if tx.send(channel_msg).await.is_err() {
                        tracing::warn!("DingTalk: message channel closed");
                        break;
                    }
                }
                _ => {}
            }
        }

        anyhow::bail!("DingTalk WebSocket stream ended")
    }

    async fn health_check(&self) -> bool {
        self.register_connection().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec![],
            crate::config::schema::DingTalkMessageType::Markdown,
            None,
            None,
            "content".into(),
        );
        assert_eq!(ch.name(), "dingtalk");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["*".into()],
            crate::config::schema::DingTalkMessageType::Markdown,
            None,
            None,
            "content".into(),
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["user123".into()],
            crate::config::schema::DingTalkMessageType::Markdown,
            None,
            None,
            "content".into(),
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec![],
            crate::config::schema::DingTalkMessageType::Markdown,
            None,
            None,
            "content".into(),
        );
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
client_id = "app_id_123"
client_secret = "secret_456"
allowed_users = ["user1", "*"]
message_type = "card"
card_template_id = "tpl-1"
card_template_key = "reply"
robot_code = "robot-abc"
"#;
        let config: crate::config::schema::DingTalkConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.client_id, "app_id_123");
        assert_eq!(config.client_secret, "secret_456");
        assert_eq!(config.allowed_users, vec!["user1", "*"]);
        assert_eq!(
            config.message_type,
            crate::config::schema::DingTalkMessageType::Card
        );
        assert_eq!(config.card_template_id.as_deref(), Some("tpl-1"));
        assert_eq!(config.card_template_key, "reply");
        assert_eq!(config.robot_code.as_deref(), Some("robot-abc"));
    }

    #[test]
    fn test_config_serde_defaults() {
        let toml_str = r#"
client_id = "id"
client_secret = "secret"
"#;
        let config: crate::config::schema::DingTalkConfig = toml::from_str(toml_str).unwrap();
        assert!(config.allowed_users.is_empty());
        assert_eq!(
            config.message_type,
            crate::config::schema::DingTalkMessageType::Markdown
        );
        assert!(config.card_template_id.is_none());
        assert_eq!(config.card_template_key, "content");
        assert!(config.robot_code.is_none());
    }

    #[test]
    fn supports_draft_updates_only_for_valid_card_mode() {
        let markdown = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["*".into()],
            crate::config::schema::DingTalkMessageType::Markdown,
            None,
            None,
            "content".into(),
        );
        assert!(!markdown.supports_draft_updates());

        let missing_template = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["*".into()],
            crate::config::schema::DingTalkMessageType::Card,
            None,
            None,
            "content".into(),
        );
        assert!(!missing_template.supports_draft_updates());

        let card = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["*".into()],
            crate::config::schema::DingTalkMessageType::Card,
            Some("tpl-1".into()),
            Some("robot-1".into()),
            "reply".into(),
        );
        assert!(card.supports_draft_updates());
    }

    #[test]
    fn parse_stream_data_supports_string_payload() {
        let frame = serde_json::json!({
            "data": "{\"text\":{\"content\":\"hello\"}}"
        });
        let parsed = DingTalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.get("content")),
            Some(&serde_json::json!("hello"))
        );
    }

    #[test]
    fn parse_stream_data_supports_object_payload() {
        let frame = serde_json::json!({
            "data": {"text": {"content": "hello"}}
        });
        let parsed = DingTalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.get("content")),
            Some(&serde_json::json!("hello"))
        );
    }

    #[test]
    fn resolve_chat_id_handles_numeric_group_conversation_type() {
        let data = serde_json::json!({
            "conversationType": 2,
            "conversationId": "cid-group",
        });
        let chat_id = DingTalkChannel::resolve_chat_id(&data, "staff-1");
        assert_eq!(chat_id, "cid-group");
    }

    #[tokio::test]
    async fn update_draft_skips_unknown_message_id() {
        let card = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["*".into()],
            crate::config::schema::DingTalkMessageType::Card,
            Some("tpl-1".into()),
            None,
            "content".into(),
        );

        let result = card
            .update_draft("cid-group", "unknown-draft", "partial")
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[tokio::test]
    async fn finalize_draft_skips_recipient_mismatch() {
        let card = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["*".into()],
            crate::config::schema::DingTalkMessageType::Card,
            Some("tpl-1".into()),
            None,
            "content".into(),
        );
        {
            let mut drafts = card.draft_states.write().await;
            drafts.insert(
                "draft-1".to_string(),
                CardDraftState {
                    recipient: "cid-group-a".to_string(),
                    last_content: "hello".to_string(),
                },
            );
        }

        let result = card
            .finalize_draft("cid-group-b", "draft-1", "final")
            .await;

        assert!(result.is_ok());
        let drafts = card.draft_states.read().await;
        assert!(drafts.contains_key("draft-1"));
    }

    #[tokio::test]
    async fn cancel_draft_skips_recipient_mismatch() {
        let card = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["*".into()],
            crate::config::schema::DingTalkMessageType::Card,
            Some("tpl-1".into()),
            None,
            "content".into(),
        );
        {
            let mut drafts = card.draft_states.write().await;
            drafts.insert(
                "draft-1".to_string(),
                CardDraftState {
                    recipient: "cid-group-a".to_string(),
                    last_content: "hello".to_string(),
                },
            );
        }

        let result = card.cancel_draft("cid-group-b", "draft-1").await;

        assert!(result.is_ok());
        let drafts = card.draft_states.read().await;
        assert!(drafts.contains_key("draft-1"));
    }

    #[tokio::test]
    async fn lookup_session_webhook_returns_cloned_url() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            vec!["*".into()],
            crate::config::schema::DingTalkMessageType::Markdown,
            None,
            None,
            "content".into(),
        );

        {
            let mut webhooks = ch.session_webhooks.write().await;
            webhooks.insert("chat-1".to_string(), "https://example.com/hook".to_string());
        }

        let url = ch.lookup_session_webhook("chat-1").await.unwrap();
        assert_eq!(url, "https://example.com/hook");
    }
}
