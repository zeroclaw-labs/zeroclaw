use async_trait::async_trait;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// WeCom (WeChat Enterprise) Bot Webhook channel.
///
/// Sends messages via the WeCom Bot Webhook API. Incoming messages are received
/// through a configurable callback URL that WeCom posts to.
pub struct WeComChannel {
    webhook_key: String,
    #[allow(dead_code)] // used by is_user_allowed which is WIP
    allowed_users: Vec<String>,
}

impl WeComChannel {
    pub fn new(webhook_key: String, allowed_users: Vec<String>) -> Self {
        Self {
            webhook_key,
            allowed_users,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.wecom")
    }

    fn webhook_url(&self) -> String {
        format!(
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key={}",
            self.webhook_key
        )
    }

    #[cfg(test)]
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }
}

#[async_trait]
impl Channel for WeComChannel {
    fn name(&self) -> &str {
        "wecom"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "msgtype": "text",
            "text": {
                "content": message.content,
            }
        });

        let resp = self
            .http_client()
            .post(self.webhook_url())
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("WeCom webhook send failed ({status}): {err}");
        }

        // WeCom returns {"errcode":0,"errmsg":"ok"} on success.
        let result: serde_json::Value = resp.json().await?;
        let errcode = result.get("errcode").and_then(|v| v.as_i64()).unwrap_or(-1);
        if errcode != 0 {
            let errmsg = result
                .get("errmsg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("WeCom API error (errcode={errcode}): {errmsg}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // WeCom Bot Webhook is send-only by default. For receiving messages,
        // an enterprise application with a callback URL is needed, which is
        // handled via the gateway webhook subsystem.
        //
        // This listener keeps the channel alive and waits for the sender to close.
        tracing::info!("WeCom: channel ready (send-only via Bot Webhook)");
        tx.closed().await;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        // Verify we can reach the WeCom API endpoint.
        let resp = self
            .http_client()
            .post(self.webhook_url())
            .json(&serde_json::json!({
                "msgtype": "text",
                "text": {
                    "content": "health_check"
                }
            }))
            .send()
            .await;

        match resp {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let ch = WeComChannel::new("test-key".into(), vec![]);
        assert_eq!(ch.name(), "wecom");
    }

    #[test]
    fn test_webhook_url() {
        let ch = WeComChannel::new("abc-123".into(), vec![]);
        assert_eq!(
            ch.webhook_url(),
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abc-123"
        );
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = WeComChannel::new("key".into(), vec!["*".into()]);
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = WeComChannel::new("key".into(), vec!["user123".into()]);
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = WeComChannel::new("key".into(), vec![]);
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
webhook_key = "key-abc-123"
allowed_users = ["user1", "*"]
"#;
        let config: zeroclaw_config::schema::WeComConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.webhook_key, "key-abc-123");
        assert_eq!(config.allowed_users, vec!["user1", "*"]);
    }

    #[test]
    fn test_config_serde_defaults() {
        let toml_str = r#"
webhook_key = "key"
"#;
        let config: zeroclaw_config::schema::WeComConfig = toml::from_str(toml_str).unwrap();
        assert!(config.allowed_users.is_empty());
    }
}
