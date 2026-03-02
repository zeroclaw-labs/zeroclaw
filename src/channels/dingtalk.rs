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

/// Cached access token with expiry time
#[derive(Clone)]
struct AccessToken {
    token: String,
    expires_at: Instant,
}

/// DingTalk channel — connects via Stream Mode WebSocket for real-time messages.
/// Replies are sent through DingTalk Open API (no session webhook required).
pub struct DingTalkChannel {
    client_id: String,
    client_secret: String,
    allowed_users: Vec<String>,
    /// Per-chat session webhooks for sending replies (chatID -> webhook URL).
    /// DingTalk provides a unique webhook URL with each incoming message.
    session_webhooks: Arc<RwLock<HashMap<String, String>>>,
    /// Cached access token for Open API calls
    access_token: Arc<RwLock<Option<AccessToken>>>,
}

/// Response from DingTalk gateway connection registration.
#[derive(serde::Deserialize)]
struct GatewayResponse {
    endpoint: String,
    ticket: String,
}

impl DingTalkChannel {
    pub fn new(client_id: String, client_secret: String, allowed_users: Vec<String>) -> Self {
        Self {
            client_id,
            client_secret,
            allowed_users,
            session_webhooks: Arc::new(RwLock::new(HashMap::new())),
            access_token: Arc::new(RwLock::new(None)),
        }
    }

    /// Get or refresh access token using OAuth2
    async fn get_access_token(&self) -> anyhow::Result<String> {
        {
            let cached = self.access_token.read().await;
            if let Some(ref at) = *cached {
                if at.expires_at > Instant::now() {
                    return Ok(at.token.clone());
                }
            }
        }

        // Re-check under write lock to avoid duplicate token fetches under contention.
        let mut cached = self.access_token.write().await;
        if let Some(ref at) = *cached {
            if at.expires_at > Instant::now() {
                return Ok(at.token.clone());
            }
        }

        let url = "https://api.dingtalk.com/v1.0/oauth2/accessToken";
        let body = serde_json::json!({
            "appKey": self.client_id,
            "appSecret": self.client_secret,
        });

        let resp = self.http_client().post(url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("DingTalk access token request failed ({status}): {err}");
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct TokenResponse {
            access_token: String,
            expire_in: u64,
        }

        let token_resp: TokenResponse = resp.json().await?;
        let expires_in = Duration::from_secs(token_resp.expire_in.saturating_sub(60));
        let token = token_resp.access_token;

        *cached = Some(AccessToken {
            token: token.clone(),
            expires_at: Instant::now() + expires_in,
        });

        Ok(token)
    }

    fn is_group_recipient(recipient: &str) -> bool {
        // DingTalk group conversation IDs are typically prefixed with `cid`.
        recipient.starts_with("cid")
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.dingtalk")
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    fn parse_stream_data(frame: &serde_json::Value) -> Option<serde_json::Value> {
        match frame.get("data") {
            Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
            Some(serde_json::Value::Object(_)) => frame.get("data").cloned(),
            _ => None,
        }
    }

    fn extract_text_content(data: &serde_json::Value) -> Option<String> {
        fn normalize_text(raw: &str) -> Option<String> {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }

        fn text_content_from_value(value: &serde_json::Value) -> Option<String> {
            match value {
                serde_json::Value::String(s) => {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                        // Some DingTalk events encode nested text payloads as JSON strings.
                        if let Some(content) = parsed
                            .get("content")
                            .and_then(|v| v.as_str())
                            .and_then(normalize_text)
                        {
                            return Some(content);
                        }
                    }
                    normalize_text(s)
                }
                serde_json::Value::Object(map) => map
                    .get("content")
                    .and_then(|v| v.as_str())
                    .and_then(normalize_text),
                _ => None,
            }
        }

        fn collect_rich_text_fragments(
            value: &serde_json::Value,
            out: &mut Vec<String>,
            depth: usize,
        ) {
            const MAX_RICH_TEXT_DEPTH: usize = 16;
            if depth >= MAX_RICH_TEXT_DEPTH {
                return;
            }

            match value {
                serde_json::Value::String(s) => {
                    if let Some(normalized) = normalize_text(s) {
                        out.push(normalized);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        collect_rich_text_fragments(item, out, depth + 1);
                    }
                }
                serde_json::Value::Object(map) => {
                    for key in ["text", "content"] {
                        if let Some(text_val) = map.get(key).and_then(|v| v.as_str()) {
                            if let Some(normalized) = normalize_text(text_val) {
                                out.push(normalized);
                            }
                        }
                    }
                    for key in ["children", "elements", "richText", "rich_text"] {
                        if let Some(child) = map.get(key) {
                            collect_rich_text_fragments(child, out, depth + 1);
                        }
                    }
                }
                _ => {}
            }
        }

        // Canonical text payload.
        if let Some(content) = data.get("text").and_then(text_content_from_value) {
            return Some(content);
        }

        // Some events include top-level content directly.
        if let Some(content) = data
            .get("content")
            .and_then(|v| v.as_str())
            .and_then(normalize_text)
        {
            return Some(content);
        }

        // Rich text payload fallback.
        if let Some(rich) = data.get("richText").or_else(|| data.get("rich_text")) {
            let mut fragments = Vec::new();
            collect_rich_text_fragments(rich, &mut fragments, 0);
            if !fragments.is_empty() {
                let merged = fragments.join(" ");
                if let Some(content) = normalize_text(&merged) {
                    return Some(content);
                }
            }
        }

        // Markdown payload fallback.
        data.get("markdown")
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str())
            .and_then(normalize_text)
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

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;

        let title = message.subject.as_deref().unwrap_or("ZeroClaw");

        let msg_param = serde_json::json!({
            "text": message.content,
            "title": title,
        });

        let (url, body) = if Self::is_group_recipient(&message.recipient) {
            (
                "https://api.dingtalk.com/v1.0/robot/groupMessages/send",
                serde_json::json!({
                    "robotCode": self.client_id,
                    "openConversationId": message.recipient,
                    "msgKey": "sampleMarkdown",
                    "msgParam": msg_param.to_string(),
                }),
            )
        } else {
            (
                "https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend",
                serde_json::json!({
                    "robotCode": self.client_id,
                    "userIds": [&message.recipient],
                    "msgKey": "sampleMarkdown",
                    "msgParam": msg_param.to_string(),
                }),
            )
        };

        let resp = self
            .http_client()
            .post(url)
            .header("x-acs-dingtalk-access-token", &token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let resp_text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&resp_text);
            anyhow::bail!("DingTalk API send failed ({status}): {sanitized}");
        }

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&resp_text) {
            let app_code = json
                .get("errcode")
                .and_then(|v| v.as_i64())
                .or_else(|| json.get("code").and_then(|v| v.as_i64()))
                .unwrap_or(0);
            if app_code != 0 {
                let app_msg = json
                    .get("errmsg")
                    .and_then(|v| v.as_str())
                    .or_else(|| json.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("unknown error");
                anyhow::bail!("DingTalk API send rejected (code={app_code}): {app_msg}");
            }
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
                    let Some(content) = Self::extract_text_content(&data) else {
                        let keys = data
                            .as_object()
                            .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                            .unwrap_or_default();
                        let msg_type = data.get("msgtype").and_then(|v| v.as_str()).unwrap_or("");
                        tracing::warn!(
                            msg_type = %msg_type,
                            keys = ?keys,
                            "DingTalk: dropped callback without extractable text content"
                        );
                        continue;
                    };

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
                        content,
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
        let ch = DingTalkChannel::new("id".into(), "secret".into(), vec![]);
        assert_eq!(ch.name(), "dingtalk");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = DingTalkChannel::new("id".into(), "secret".into(), vec!["*".into()]);
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = DingTalkChannel::new("id".into(), "secret".into(), vec!["user123".into()]);
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = DingTalkChannel::new("id".into(), "secret".into(), vec![]);
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
client_id = "app_id_123"
client_secret = "secret_456"
allowed_users = ["user1", "*"]
"#;
        let config: crate::config::schema::DingTalkConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.client_id, "app_id_123");
        assert_eq!(config.client_secret, "secret_456");
        assert_eq!(config.allowed_users, vec!["user1", "*"]);
    }

    #[test]
    fn test_config_serde_defaults() {
        let toml_str = r#"
client_id = "id"
client_secret = "secret"
"#;
        let config: crate::config::schema::DingTalkConfig = toml::from_str(toml_str).unwrap();
        assert!(config.allowed_users.is_empty());
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

    #[test]
    fn extract_text_content_prefers_nested_text_content() {
        let data = serde_json::json!({
            "text": {"content": "  你好，世界  "},
            "content": "fallback",
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&data).as_deref(),
            Some("你好，世界")
        );
    }

    #[test]
    fn extract_text_content_supports_json_encoded_text_string() {
        let data = serde_json::json!({
            "text": "{\"content\":\"中文消息\"}"
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&data).as_deref(),
            Some("中文消息")
        );
    }

    #[test]
    fn extract_text_content_falls_back_to_content_and_markdown() {
        let direct = serde_json::json!({
            "content": "  direct payload  "
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&direct).as_deref(),
            Some("direct payload")
        );

        let markdown = serde_json::json!({
            "markdown": {"text": "  markdown body  "}
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&markdown).as_deref(),
            Some("markdown body")
        );
    }

    #[test]
    fn extract_text_content_supports_rich_text_payload() {
        let data = serde_json::json!({
            "richText": [
                {"text": "现在"},
                {"content": "呢？"}
            ]
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&data).as_deref(),
            Some("现在 呢？")
        );
    }

    #[test]
    fn extract_text_content_bounds_rich_text_recursion_depth() {
        let mut deep = serde_json::json!({"text": "deep-content"});
        for _ in 0..24 {
            deep = serde_json::json!({"children": [deep]});
        }
        let data = serde_json::json!({"richText": deep});

        assert_eq!(DingTalkChannel::extract_text_content(&data), None);
    }
}
