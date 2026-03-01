use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::schema::NapcatConfig;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

const NAPCAT_SEND_PRIVATE: &str = "/send_private_msg";
const NAPCAT_SEND_GROUP: &str = "/send_group_msg";
const NAPCAT_STATUS: &str = "/get_status";
const NAPCAT_DEDUP_CAPACITY: usize = 10_000;
const NAPCAT_MAX_BACKOFF_SECS: u64 = 60;

fn current_unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn normalize_token(raw: &str) -> Option<String> {
    let token = raw.trim();
    (!token.is_empty()).then(|| token.to_string())
}

fn derive_api_base_from_websocket(websocket_url: &str) -> Option<String> {
    let mut url = Url::parse(websocket_url).ok()?;
    match url.scheme() {
        "ws" => {
            url.set_scheme("http").ok()?;
        }
        "wss" => {
            url.set_scheme("https").ok()?;
        }
        _ => return None,
    }
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string().trim_end_matches('/').to_string())
}

fn compose_onebot_content(content: &str, reply_message_id: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(reply_id) = reply_message_id {
        let trimmed = reply_id.trim();
        if !trimmed.is_empty() {
            parts.push(format!("[CQ:reply,id={trimmed}]"));
        }
    }

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(marker) = trimmed
            .strip_prefix("[IMAGE:")
            .and_then(|v| v.strip_suffix(']'))
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            parts.push(format!("[CQ:image,file={marker}]"));
            continue;
        }
        parts.push(line.to_string());
    }

    parts.join("\n").trim().to_string()
}

fn parse_message_segments(message: &Value) -> String {
    if let Some(text) = message.as_str() {
        return text.trim().to_string();
    }

    let Some(segments) = message.as_array() else {
        return String::new();
    };

    let mut parts = Vec::new();
    for segment in segments {
        let seg_type = segment
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let data = segment.get("data");
        match seg_type {
            "text" => {
                if let Some(text) = data
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    parts.push(text.to_string());
                }
            }
            "image" => {
                if let Some(url) = data
                    .and_then(|d| d.get("url"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    parts.push(format!("[IMAGE:{url}]"));
                } else if let Some(file) = data
                    .and_then(|d| d.get("file"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    parts.push(format!("[IMAGE:{file}]"));
                }
            }
            _ => {}
        }
    }

    parts.join("\n").trim().to_string()
}

fn extract_message_id(event: &Value) -> String {
    event
        .get("message_id")
        .and_then(Value::as_i64)
        .map(|v| v.to_string())
        .or_else(|| {
            event
                .get("message_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn extract_timestamp(event: &Value) -> u64 {
    event
        .get("time")
        .and_then(Value::as_i64)
        .and_then(|v| u64::try_from(v).ok())
        .unwrap_or_else(current_unix_timestamp_secs)
}

pub struct NapcatChannel {
    websocket_url: String,
    api_base_url: String,
    access_token: Option<String>,
    allowed_users: Vec<String>,
    dedup: Arc<RwLock<HashSet<String>>>,
}

impl NapcatChannel {
    pub fn from_config(config: NapcatConfig) -> Result<Self> {
        let websocket_url = config.websocket_url.trim().to_string();
        if websocket_url.is_empty() {
            anyhow::bail!("napcat.websocket_url cannot be empty");
        }

        let api_base_url = if config.api_base_url.trim().is_empty() {
            derive_api_base_from_websocket(&websocket_url).ok_or_else(|| {
                anyhow!("napcat.api_base_url is required when websocket_url is not ws:// or wss://")
            })?
        } else {
            config.api_base_url.trim().trim_end_matches('/').to_string()
        };

        Ok(Self {
            websocket_url,
            api_base_url,
            access_token: normalize_token(config.access_token.as_deref().unwrap_or_default()),
            allowed_users: config.allowed_users,
            dedup: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    async fn is_duplicate(&self, message_id: &str) -> bool {
        if message_id.is_empty() {
            return false;
        }
        let mut dedup = self.dedup.write().await;
        if dedup.contains(message_id) {
            return true;
        }
        if dedup.len() >= NAPCAT_DEDUP_CAPACITY {
            let remove_n = dedup.len() / 2;
            let to_remove: Vec<String> = dedup.iter().take(remove_n).cloned().collect();
            for key in to_remove {
                dedup.remove(&key);
            }
        }
        dedup.insert(message_id.to_string());
        false
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.napcat")
    }

    async fn post_onebot(&self, endpoint: &str, body: &Value) -> Result<()> {
        let url = format!("{}{}", self.api_base_url, endpoint);
        let mut request = self.http_client().post(&url).json(body);
        if let Some(token) = &self.access_token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let err = response.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("Napcat HTTP request failed ({status}): {sanitized}");
        }

        let payload: Value = response.json().await.unwrap_or_else(|_| json!({}));
        if payload
            .get("retcode")
            .and_then(Value::as_i64)
            .is_some_and(|retcode| retcode != 0)
        {
            let msg = payload
                .get("wording")
                .and_then(Value::as_str)
                .or_else(|| payload.get("msg").and_then(Value::as_str))
                .unwrap_or("unknown error");
            anyhow::bail!("Napcat returned retcode != 0: {msg}");
        }

        Ok(())
    }

    fn build_ws_request(&self) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
        let mut ws_url =
            Url::parse(&self.websocket_url).with_context(|| "invalid napcat.websocket_url")?;
        if let Some(token) = &self.access_token {
            let has_access_token = ws_url.query_pairs().any(|(k, _)| k == "access_token");
            if !has_access_token {
                ws_url.query_pairs_mut().append_pair("access_token", token);
            }
        }

        let mut request = ws_url.as_str().into_client_request()?;
        if let Some(token) = &self.access_token {
            let value = format!("Bearer {token}");
            request.headers_mut().insert(
                tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
                value
                    .parse()
                    .context("invalid napcat access token header")?,
            );
        }
        Ok(request)
    }

    async fn parse_message_event(&self, event: &Value) -> Option<ChannelMessage> {
        if event.get("post_type").and_then(Value::as_str) != Some("message") {
            return None;
        }

        let message_id = extract_message_id(event);
        if self.is_duplicate(&message_id).await {
            return None;
        }

        let message_type = event
            .get("message_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let sender_id = event
            .get("user_id")
            .and_then(Value::as_i64)
            .map(|v| v.to_string())
            .or_else(|| {
                event
                    .get("sender")
                    .and_then(|s| s.get("user_id"))
                    .and_then(Value::as_i64)
                    .map(|v| v.to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        if !self.is_user_allowed(&sender_id) {
            tracing::warn!("Napcat: ignoring message from unauthorized user: {sender_id}");
            return None;
        }

        let content = {
            let parsed = parse_message_segments(event.get("message").unwrap_or(&Value::Null));
            if parsed.is_empty() {
                event
                    .get("raw_message")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("")
                    .to_string()
            } else {
                parsed
            }
        };

        if content.trim().is_empty() {
            return None;
        }

        let reply_target = if message_type == "group" {
            let group_id = event
                .get("group_id")
                .and_then(Value::as_i64)
                .map(|v| v.to_string())
                .unwrap_or_default();
            format!("group:{group_id}")
        } else {
            format!("user:{sender_id}")
        };

        Some(ChannelMessage {
            id: message_id.clone(),
            sender: sender_id,
            reply_target,
            content,
            channel: "napcat".to_string(),
            timestamp: extract_timestamp(event),
            // This is a message id for passive reply, not a thread id.
            thread_ts: Some(message_id),
        })
    }

    async fn listen_once(&self, tx: &tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let request = self.build_ws_request()?;
        let (mut socket, _) = connect_async(request).await?;
        tracing::info!("Napcat: connected to {}", self.websocket_url);

        while let Some(frame) = socket.next().await {
            match frame {
                Ok(Message::Text(text)) => {
                    let event: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(err) => {
                            tracing::warn!("Napcat: failed to parse event payload: {err}");
                            continue;
                        }
                    };
                    if let Some(msg) = self.parse_message_event(&event).await {
                        if tx.send(msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Ok(Message::Binary(_)) => {}
                Ok(Message::Ping(payload)) => {
                    socket.send(Message::Pong(payload)).await?;
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(frame)) => {
                    return Err(anyhow!("Napcat websocket closed: {:?}", frame));
                }
                Ok(Message::Frame(_)) => {}
                Err(err) => {
                    return Err(anyhow!("Napcat websocket error: {err}"));
                }
            }
        }

        Err(anyhow!("Napcat websocket stream ended"))
    }
}

#[async_trait]
impl Channel for NapcatChannel {
    fn name(&self) -> &str {
        "napcat"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let payload = compose_onebot_content(&message.content, message.thread_ts.as_deref());
        if payload.trim().is_empty() {
            return Ok(());
        }

        if let Some(group_id) = message.recipient.strip_prefix("group:") {
            let body = json!({
                "group_id": group_id,
                "message": payload,
            });
            self.post_onebot(NAPCAT_SEND_GROUP, &body).await?;
            return Ok(());
        }

        let user_id = message
            .recipient
            .strip_prefix("user:")
            .unwrap_or(&message.recipient)
            .trim();
        if user_id.is_empty() {
            anyhow::bail!("Napcat recipient is empty");
        }

        let body = json!({
            "user_id": user_id,
            "message": payload,
        });
        self.post_onebot(NAPCAT_SEND_PRIVATE, &body).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.listen_once(&tx).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::error!(
                        "Napcat listener error: {err}. Reconnecting in {:?}...",
                        backoff
                    );
                    sleep(backoff).await;
                    backoff =
                        std::cmp::min(backoff * 2, Duration::from_secs(NAPCAT_MAX_BACKOFF_SECS));
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}{}", self.api_base_url, NAPCAT_STATUS);
        let mut request = self.http_client().get(url);
        if let Some(token) = &self.access_token {
            request = request.bearer_auth(token);
        }
        request
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map(|resp| resp.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_api_base_converts_ws_to_http() {
        let base = derive_api_base_from_websocket("ws://127.0.0.1:3001/ws").unwrap();
        assert_eq!(base, "http://127.0.0.1:3001");
    }

    #[test]
    fn compose_onebot_content_includes_reply_and_image_markers() {
        let content = "hello\n[IMAGE:https://example.com/cat.png]";
        let parsed = compose_onebot_content(content, Some("123"));
        assert!(parsed.contains("[CQ:reply,id=123]"));
        assert!(parsed.contains("[CQ:image,file=https://example.com/cat.png]"));
        assert!(parsed.contains("hello"));
    }

    #[tokio::test]
    async fn parse_private_event_maps_to_channel_message() {
        let cfg = NapcatConfig {
            websocket_url: "ws://127.0.0.1:3001".into(),
            api_base_url: "".into(),
            access_token: None,
            allowed_users: vec!["10001".into()],
        };
        let channel = NapcatChannel::from_config(cfg).unwrap();
        let event = json!({
            "post_type": "message",
            "message_type": "private",
            "message_id": 99,
            "user_id": 10001,
            "time": 1700000000,
            "message": [{"type":"text","data":{"text":"hi"}}]
        });

        let msg = channel.parse_message_event(&event).await.unwrap();
        assert_eq!(msg.channel, "napcat");
        assert_eq!(msg.sender, "10001");
        assert_eq!(msg.reply_target, "user:10001");
        assert_eq!(msg.content, "hi");
        assert_eq!(msg.thread_ts.as_deref(), Some("99"));
    }

    #[tokio::test]
    async fn parse_group_event_with_image_segment() {
        let cfg = NapcatConfig {
            websocket_url: "ws://127.0.0.1:3001".into(),
            api_base_url: "".into(),
            access_token: None,
            allowed_users: vec!["*".into()],
        };
        let channel = NapcatChannel::from_config(cfg).unwrap();
        let event = json!({
            "post_type": "message",
            "message_type": "group",
            "message_id": "abc-1",
            "user_id": 20002,
            "group_id": 30003,
            "message": [
                {"type":"text","data":{"text":"photo"}},
                {"type":"image","data":{"url":"https://img.example.com/1.jpg"}}
            ]
        });

        let msg = channel.parse_message_event(&event).await.unwrap();
        assert_eq!(msg.reply_target, "group:30003");
        assert!(msg.content.contains("photo"));
        assert!(msg
            .content
            .contains("[IMAGE:https://img.example.com/1.jpg]"));
    }
}
