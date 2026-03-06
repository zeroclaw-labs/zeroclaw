use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

pub struct SynoBotChatChannel {
    base_url: String,
    token: String,
    allowed_user_ids: Vec<String>,
    client: reqwest::Client,
}

impl SynoBotChatChannel {
    pub fn new(base_url: String, token: String, allowed_user_ids: Vec<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            allowed_user_ids,
            client: crate::config::build_runtime_proxy_client("channel.synobotchat"),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_user_ids
            .iter()
            .any(|u| u == "*" || u == user_id)
    }

    fn now_unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn parse_timestamp_secs(value: Option<&String>) -> u64 {
        let raw = value
            .map(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(None)
            .unwrap_or_else(Self::now_unix_secs);
        if raw > 1_000_000_000_000 {
            raw / 1000
        } else {
            raw
        }
    }

    fn build_file_url(&self, post_id: &str) -> String {
        let token = urlencoding::encode(&self.token);
        let post_id = urlencoding::encode(post_id);
        format!(
            "{}/webapi/entry.cgi?api=SYNO.Chat.External&method=post_file_get&version=2&token={token}&post_id={post_id}",
            self.base_url
        )
    }

    pub fn parse_webhook_form(&self, form: &HashMap<String, String>) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();
        let Some(user_id) = form
            .get("user_id")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        else {
            return messages;
        };

        if !self.is_user_allowed(user_id) {
            tracing::warn!(
                "SynoBotChat: ignoring message from unauthorized user: {user_id}. \
                Add to channels_config.synobotchat.allowed_user_ids in config.toml."
            );
            return messages;
        }

        let username = form
            .get("username")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let post_id = form
            .get("post_id")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        let mut content = form
            .get("text")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string());
        if content.is_none() {
            if let Some(post_id) = post_id {
                let file_url = self.build_file_url(post_id);
                content = Some(format!("File: {file_url}"));
            }
        }
        let Some(content) = content else {
            return messages;
        };

        let message_id = post_id
            .map(|v| v.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let timestamp = Self::parse_timestamp_secs(form.get("timestamp"));
        let thread_ts = form
            .get("thread_id")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty() && *v != "0")
            .map(|v| v.to_string());

        let sender = username.unwrap_or(user_id);
        messages.push(ChannelMessage {
            id: message_id,
            reply_target: user_id.to_string(),
            sender: sender.to_string(),
            content,
            channel: "synobotchat".to_string(),
            timestamp,
            thread_ts,
        });

        messages
    }
}

#[async_trait]
impl Channel for SynoBotChatChannel {
    fn name(&self) -> &str {
        "synobotchat"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let recipient = message.recipient.trim();
        if recipient.is_empty() {
            anyhow::bail!("SynoBotChat recipient is empty");
        }
        let user_id_value = recipient
            .parse::<u64>()
            .map(serde_json::Value::from)
            .unwrap_or_else(|_| serde_json::Value::String(recipient.to_string()));
        let payload = serde_json::json!({
            "text": message.content,
            "user_ids": [user_id_value]
        });
        let token = urlencoding::encode(&self.token);
        let url = format!(
            "{}/webapi/entry.cgi?api=SYNO.Chat.External&method=chatbot&version=2&token={token}",
            self.base_url
        );
        let resp = self
            .client
            .post(&url)
            .form(&[("payload", payload.to_string())])
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&body);
            tracing::error!("SynoBotChat send failed: {status} — {sanitized}");
            anyhow::bail!("SynoBotChat API error: {status}");
        }
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!(
            "SynoBotChat channel active (webhook mode). Configure Synology Chat bot to POST to /synobotchat."
        );
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }
}
