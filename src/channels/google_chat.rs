//! Google Chat channel integration.
//!
//! Supports two modes:
//! - **Webhook**: Send-only via incoming webhook URL
//! - **API**: Full bidirectional via service account token + space ID
//!
//! Config:
//! ```toml
//! [channels_config.google_chat]
//! token = "$GOOGLE_CHAT_BOT_TOKEN"
//! space = "spaces/AAAA..."
//! webhook_url = "https://chat.googleapis.com/v1/spaces/..."
//! ```

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

const API_BASE: &str = "https://chat.googleapis.com/v1";

pub struct GoogleChatChannelConfig {
    pub token: Option<String>,
    pub webhook_url: Option<String>,
    pub space: Option<String>,
}

pub struct GoogleChatChannel {
    token: Option<String>,
    webhook_url: Option<String>,
    space: Option<String>,
    client: reqwest::Client,
    seen: Arc<Mutex<HashSet<String>>>,
}

impl GoogleChatChannel {
    pub fn new(cfg: GoogleChatChannelConfig) -> Self {
        Self {
            token: cfg.token,
            webhook_url: cfg.webhook_url,
            space: cfg.space,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            seen: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn has_api_mode(&self) -> bool {
        self.token.is_some() && self.space.is_some()
    }
}

#[async_trait]
impl Channel for GoogleChatChannel {
    fn name(&self) -> &str {
        "google-chat"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let body = serde_json::json!({ "text": message.content });

        if let Some(ref webhook_url) = self.webhook_url {
            self.client
                .post(webhook_url)
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
            return Ok(());
        }

        if let (Some(ref token), Some(ref space)) = (&self.token, &self.space) {
            let url = format!("{API_BASE}/{space}/messages");
            self.client
                .post(&url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
            return Ok(());
        }

        anyhow::bail!("Google Chat: no webhook_url or token+space configured")
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        if !self.has_api_mode() {
            tracing::info!("Google Chat: webhook-only mode, no polling");
            // In webhook mode, messages arrive via the gateway webhook endpoint.
            // Keep the task alive so the channel isn't dropped.
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        }

        let token = self.token.as_deref().unwrap();
        let space = self.space.as_deref().unwrap();
        let url = format!("{API_BASE}/{space}/messages?pageSize=20");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            let resp = match self.client.get(&url).bearer_auth(token).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("Google Chat poll error: {e}");
                    continue;
                }
            };

            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(_) => continue,
            };

            let messages = body["messages"].as_array();
            let Some(messages) = messages else { continue };

            let mut seen = self.seen.lock().await;

            for msg in messages.iter().rev() {
                let name = msg["name"].as_str().unwrap_or_default();
                if name.is_empty() || seen.contains(name) {
                    continue;
                }

                // Skip bot messages
                if msg["sender"]["type"].as_str() == Some("BOT") {
                    seen.insert(name.to_string());
                    continue;
                }

                let text = msg["text"].as_str().unwrap_or_default().trim();
                if text.is_empty() {
                    seen.insert(name.to_string());
                    continue;
                }

                let sender = msg["sender"]["displayName"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();

                let thread_name = msg["thread"]["name"].as_str().map(String::from);

                seen.insert(name.to_string());

                // Cap seen set
                if seen.len() > 1000 {
                    let excess: Vec<_> = seen.iter().take(500).cloned().collect();
                    for k in excess {
                        seen.remove(&k);
                    }
                }

                let channel_msg = ChannelMessage {
                    id: name.to_string(),
                    sender,
                    reply_target: space.to_string(),
                    content: text.to_string(),
                    channel: "google-chat".to_string(),
                    timestamp: 0,
                    thread_ts: thread_name,
                    interruption_scope_id: None,
                    attachments: Vec::new(),
                };

                if tx.send(channel_msg).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        if let Some(ref webhook_url) = self.webhook_url {
            return self.client.head(webhook_url).send().await.is_ok();
        }
        if let (Some(ref token), Some(ref space)) = (&self.token, &self.space) {
            let url = format!("{API_BASE}/{space}");
            return self
                .client
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .is_ok();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_name() {
        let ch = GoogleChatChannel::new(GoogleChatChannelConfig {
            token: None,
            webhook_url: Some("https://example.com/webhook".into()),
            space: None,
        });
        assert_eq!(ch.name(), "google-chat");
    }

    #[test]
    fn api_mode_requires_token_and_space() {
        let no_api = GoogleChatChannel::new(GoogleChatChannelConfig {
            token: Some("tok".into()),
            webhook_url: None,
            space: None,
        });
        assert!(!no_api.has_api_mode());

        let api = GoogleChatChannel::new(GoogleChatChannelConfig {
            token: Some("tok".into()),
            webhook_url: None,
            space: Some("spaces/ABC".into()),
        });
        assert!(api.has_api_mode());
    }

    #[tokio::test]
    async fn send_fails_without_config() {
        let ch = GoogleChatChannel::new(GoogleChatChannelConfig {
            token: None,
            webhook_url: None,
            space: None,
        });
        let msg = SendMessage::new("hello", "recipient");
        assert!(ch.send(&msg).await.is_err());
    }
}
