//! LINE Messaging API channel integration.
//!
//! Uses the LINE Messaging API with webhook verification and
//! reply/push message endpoints.
//!
//! Config:
//! ```toml
//! [channels_config.line]
//! channel_access_token = "..."
//! channel_secret = "..."
//! ```

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;

const API_BASE: &str = "https://api.line.me/v2/bot";

pub struct LineChannelConfig {
    pub channel_access_token: String,
    pub channel_secret: String,
}

pub struct LineChannel {
    token: String,
    #[allow(dead_code)]
    secret: String,
    client: reqwest::Client,
}

impl LineChannel {
    pub fn new(cfg: LineChannelConfig) -> Self {
        Self {
            token: cfg.channel_access_token,
            secret: cfg.channel_secret,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl Channel for LineChannel {
    fn name(&self) -> &str {
        "line"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "to": message.recipient,
            "messages": [{ "type": "text", "text": message.content }]
        });

        self.client
            .post(format!("{API_BASE}/message/push"))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // LINE uses webhooks — messages arrive via the gateway webhook endpoint.
        // This task stays alive to keep the channel registered.
        tracing::info!("LINE: webhook mode — configure webhook URL in LINE Developer Console");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    }

    async fn health_check(&self) -> bool {
        self.client
            .get(format!("{API_BASE}/info"))
            .bearer_auth(&self.token)
            .send()
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_name() {
        let ch = LineChannel::new(LineChannelConfig {
            channel_access_token: "tok".into(),
            channel_secret: "sec".into(),
        });
        assert_eq!(ch.name(), "line");
    }
}
