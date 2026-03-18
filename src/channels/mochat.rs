use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Deduplication set capacity — evict half of entries when full.
const DEDUP_CAPACITY: usize = 10_000;

/// Mochat customer service channel.
///
/// Integrates with the Mochat open-source customer service platform API
/// for receiving and sending messages through its HTTP endpoints.
pub struct MochatChannel {
    api_url: String,
    api_token: String,
    allowed_users: Vec<String>,
    poll_interval_secs: u64,
    /// Message deduplication set.
    dedup: Arc<RwLock<HashSet<String>>>,
}

impl MochatChannel {
    pub fn new(
        api_url: String,
        api_token: String,
        allowed_users: Vec<String>,
        poll_interval_secs: u64,
    ) -> Self {
        Self {
            api_url: api_url.trim_end_matches('/').to_string(),
            api_token,
            allowed_users,
            poll_interval_secs,
            dedup: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.mochat")
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Check and insert message ID for deduplication.
    async fn is_duplicate(&self, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }

        let mut dedup = self.dedup.write().await;

        if dedup.contains(msg_id) {
            return true;
        }

        if dedup.len() >= DEDUP_CAPACITY {
            let to_remove: Vec<String> = dedup.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
            for key in to_remove {
                dedup.remove(&key);
            }
        }

        dedup.insert(msg_id.to_string());
        false
    }
}

#[async_trait]
impl Channel for MochatChannel {
    fn name(&self) -> &str {
        "mochat"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let body = json!({
            "toUserId": message.recipient,
            "msgType": "text",
            "content": {
                "text": message.content,
            }
        });

        let resp = self
            .http_client()
            .post(format!("{}/api/message/send", self.api_url))
            .header("Authorization", format!("Bearer {}", self.api_token))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Mochat send message failed ({status}): {err}");
        }

        let result: serde_json::Value = resp.json().await?;
        let code = result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code != 0 && code != 200 {
            let msg = result
                .get("msg")
                .or_else(|| result.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("Mochat API error (code={code}): {msg}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!("Mochat: starting message poller");

        let poll_interval = std::time::Duration::from_secs(self.poll_interval_secs);
        let mut last_message_id: Option<String> = None;

        loop {
            let mut url = format!("{}/api/message/receive", self.api_url);
            if let Some(ref id) = last_message_id {
                use std::fmt::Write;
                let _ = write!(url, "?since_id={id}");
            }

            match self
                .http_client()
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.api_token))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let data: serde_json::Value = match resp.json().await {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!("Mochat: failed to parse response: {e}");
                            tokio::time::sleep(poll_interval).await;
                            continue;
                        }
                    };

                    let messages = data
                        .get("data")
                        .or_else(|| data.get("messages"))
                        .and_then(|d| d.as_array());

                    if let Some(messages) = messages {
                        for msg in messages {
                            let msg_id = msg
                                .get("messageId")
                                .or_else(|| msg.get("id"))
                                .and_then(|i| i.as_str())
                                .unwrap_or("");

                            if self.is_duplicate(msg_id).await {
                                continue;
                            }

                            let sender = msg
                                .get("fromUserId")
                                .or_else(|| msg.get("sender"))
                                .and_then(|s| s.as_str())
                                .unwrap_or("unknown");

                            if !self.is_user_allowed(sender) {
                                tracing::debug!(
                                    "Mochat: ignoring message from unauthorized user: {sender}"
                                );
                                continue;
                            }

                            let content = msg
                                .get("content")
                                .and_then(|c| {
                                    c.get("text")
                                        .and_then(|t| t.as_str())
                                        .or_else(|| c.as_str())
                                })
                                .unwrap_or("")
                                .trim();

                            if content.is_empty() {
                                continue;
                            }

                            let channel_msg = ChannelMessage {
                                id: Uuid::new_v4().to_string(),
                                sender: sender.to_string(),
                                reply_target: sender.to_string(),
                                content: content.to_string(),
                                channel: "mochat".to_string(),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                thread_ts: None,
                                interruption_scope_id: None,
                                observe_group: false,
                            };

                            if tx.send(channel_msg).await.is_err() {
                                tracing::warn!("Mochat: message channel closed");
                                return Ok(());
                            }

                            if !msg_id.is_empty() {
                                last_message_id = Some(msg_id.to_string());
                            }
                        }
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    let err = resp.text().await.unwrap_or_default();
                    tracing::warn!("Mochat: poll request failed ({status}): {err}");
                }
                Err(e) => {
                    tracing::warn!("Mochat: poll request error: {e}");
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn health_check(&self) -> bool {
        let resp = self
            .http_client()
            .get(format!("{}/api/health", self.api_url))
            .header("Authorization", format!("Bearer {}", self.api_token))
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
        let ch = MochatChannel::new("https://mochat.example.com".into(), "tok".into(), vec![], 5);
        assert_eq!(ch.name(), "mochat");
    }

    #[test]
    fn test_api_url_trailing_slash_stripped() {
        let ch = MochatChannel::new(
            "https://mochat.example.com/".into(),
            "tok".into(),
            vec![],
            5,
        );
        assert_eq!(ch.api_url, "https://mochat.example.com");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = MochatChannel::new("https://m.test".into(), "tok".into(), vec!["*".into()], 5);
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = MochatChannel::new(
            "https://m.test".into(),
            "tok".into(),
            vec!["user123".into()],
            5,
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = MochatChannel::new("https://m.test".into(), "tok".into(), vec![], 5);
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[tokio::test]
    async fn test_dedup() {
        let ch = MochatChannel::new("https://m.test".into(), "tok".into(), vec![], 5);
        assert!(!ch.is_duplicate("msg1").await);
        assert!(ch.is_duplicate("msg1").await);
        assert!(!ch.is_duplicate("msg2").await);
    }

    #[tokio::test]
    async fn test_dedup_empty_id() {
        let ch = MochatChannel::new("https://m.test".into(), "tok".into(), vec![], 5);
        assert!(!ch.is_duplicate("").await);
        assert!(!ch.is_duplicate("").await);
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
api_url = "https://mochat.example.com"
api_token = "secret"
allowed_users = ["user1"]
"#;
        let config: crate::config::schema::MochatConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.api_url, "https://mochat.example.com");
        assert_eq!(config.api_token, "secret");
        assert_eq!(config.allowed_users, vec!["user1"]);
    }

    #[test]
    fn test_config_serde_defaults() {
        let toml_str = r#"
api_url = "https://mochat.example.com"
api_token = "secret"
"#;
        let config: crate::config::schema::MochatConfig = toml::from_str(toml_str).unwrap();
        assert!(config.allowed_users.is_empty());
        assert_eq!(config.poll_interval_secs, 5);
    }
}
