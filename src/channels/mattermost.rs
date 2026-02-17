use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::{bail, Result};
use async_trait::async_trait;

/// Mattermost channel — polls channel posts via REST API v4.
/// Mattermost is API-compatible with many Slack patterns but uses a dedicated v4 structure.
pub struct MattermostChannel {
    base_url: String, // e.g., https://mm.example.com
    bot_token: String,
    channel_id: Option<String>,
    allowed_users: Vec<String>,
    client: reqwest::Client,
}

impl MattermostChannel {
    pub fn new(
        base_url: String,
        bot_token: String,
        channel_id: Option<String>,
        allowed_users: Vec<String>,
    ) -> Self {
        // Ensure base_url doesn't have a trailing slash for consistent path joining
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            base_url,
            bot_token,
            channel_id,
            allowed_users,
            client: reqwest::Client::new(),
        }
    }

    /// Check if a user ID is in the allowlist.
    /// Empty list means deny everyone. "*" means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Get the bot's own user ID so we can ignore our own messages.
    async fn get_bot_user_id(&self) -> Option<String> {
        let resp: serde_json::Value = self
            .client
            .get(format!("{}/api/v4/users/me", self.base_url))
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;

        resp.get("id")
            .and_then(|u| u.as_str())
            .map(String::from)
    }
}

#[async_trait]
impl Channel for MattermostChannel {
    fn name(&self) -> &str {
        "mattermost"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Mattermost supports threading via 'root_id'.
        // We pack 'channel_id:root_id' into recipient if it's a thread.
        let (channel_id, root_id) = if let Some((c, r)) = message.recipient.split_once(':') {
            (c, Some(r))
        } else {
            (message.recipient.as_str(), None)
        };

        let mut body_map = serde_json::json!({
            "channel_id": channel_id,
            "message": message.content
        });

        if let Some(root) = root_id {
            body_map
                .as_object_mut()
                .unwrap()
                .insert("root_id".to_string(), serde_json::Value::String(root.to_string()));
        }

        let resp = self
            .client
            .post(format!("{}/api/v4/posts", self.base_url))
            .bearer_auth(&self.bot_token)
            .json(&body_map)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("Mattermost post failed ({status}): {body}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let channel_id = self
            .channel_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Mattermost channel_id required for listening"))?;

        let bot_user_id = self.get_bot_user_id().await.unwrap_or_default();
        #[allow(clippy::cast_possible_truncation)]
        let mut last_create_at = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()) as i64;

        tracing::info!("Mattermost channel listening on {}...", channel_id);

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;

            let resp = match self
                .client
                .get(format!(
                    "{}/api/v4/channels/{}/posts",
                    self.base_url, channel_id
                ))
                .bearer_auth(&self.bot_token)
                .query(&[("since", last_create_at.to_string())])
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Mattermost poll error: {e}");
                    continue;
                }
            };

            let data: serde_json::Value = match resp.json().await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("Mattermost parse error: {e}");
                    continue;
                }
            };

            if let Some(posts) = data.get("posts").and_then(|p| p.as_object()) {
                // Process in chronological order
                let mut post_list: Vec<_> = posts.values().collect();
                post_list.sort_by_key(|p| p.get("create_at").and_then(|c| c.as_i64()).unwrap_or(0));

                for post in post_list {
                    let msg = self.parse_mattermost_post(post, &bot_user_id, last_create_at, &channel_id);
                    let create_at = post
                        .get("create_at")
                        .and_then(|c| c.as_i64())
                        .unwrap_or(last_create_at);
                    last_create_at = last_create_at.max(create_at);

                    if let Some(channel_msg) = msg {
                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.client
            .get(format!("{}/api/v4/users/me", self.base_url))
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

impl MattermostChannel {
    fn parse_mattermost_post(
        &self,
        post: &serde_json::Value,
        bot_user_id: &str,
        last_create_at: i64,
        channel_id: &str,
    ) -> Option<ChannelMessage> {
        let id = post.get("id").and_then(|i| i.as_str()).unwrap_or("");
        let user_id = post.get("user_id").and_then(|u| u.as_str()).unwrap_or("");
        let text = post.get("message").and_then(|m| m.as_str()).unwrap_or("");
        let create_at = post.get("create_at").and_then(|c| c.as_i64()).unwrap_or(0);
        let root_id = post.get("root_id").and_then(|r| r.as_str()).unwrap_or("");

        if user_id == bot_user_id || create_at <= last_create_at || text.is_empty() {
            return None;
        }

        if !self.is_user_allowed(user_id) {
            tracing::warn!("Mattermost: ignoring message from unauthorized user: {user_id}");
            return None;
        }

        // If it's a thread, include root_id in reply_to so we reply in the same thread
        let reply_target = if !root_id.is_empty() {
            format!("{}:{}", channel_id, root_id)
        } else {
            // Or if it's a top-level message that WE want to start a thread on, 
            // the next reply will use THIS post's ID as root_id.
            // But for now, we follow Mattermost's 'reply' convention where
            // replying to a post uses its ID as root_id.
            format!("{}:{}", channel_id, id)
        };

        Some(ChannelMessage {
            id: format!("mattermost_{id}"),
            sender: user_id.to_string(),
            reply_target,
            content: text.to_string(),
            channel: "mattermost".to_string(),
            #[allow(clippy::cast_sign_loss)]
            timestamp: (create_at / 1000) as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mattermost_url_trimming() {
        let ch = MattermostChannel::new(
            "https://mm.example.com/".into(),
            "token".into(),
            None,
            vec![],
        );
        assert_eq!(ch.base_url, "https://mm.example.com");
    }

    #[test]
    fn mattermost_allowlist_wildcard() {
        let ch = MattermostChannel::new("url".into(), "token".into(), None, vec!["*".into()]);
        assert!(ch.is_user_allowed("any-id"));
    }

    #[test]
    fn mattermost_parse_post_basic() {
        let ch = MattermostChannel::new("url".into(), "token".into(), None, vec!["*".into()]);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "hello world",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(&post, "bot123", 1_500_000_000_000_i64, "chan789")
            .unwrap();
        assert_eq!(msg.sender, "user456");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.reply_target, "chan789:post123"); // Threads on the post
    }

    #[test]
    fn mattermost_parse_post_thread() {
        let ch = MattermostChannel::new("url".into(), "token".into(), None, vec!["*".into()]);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "reply",
            "create_at": 1_600_000_000_000_i64,
            "root_id": "root789"
        });

        let msg = ch
            .parse_mattermost_post(&post, "bot123", 1_500_000_000_000_i64, "chan789")
            .unwrap();
        assert_eq!(msg.reply_target, "chan789:root789"); // Stays in the thread
    }

    #[test]
    fn mattermost_parse_post_ignore_self() {
        let ch = MattermostChannel::new("url".into(), "token".into(), None, vec!["*".into()]);
        let post = json!({
            "id": "post123",
            "user_id": "bot123",
            "message": "my own message",
            "create_at": 1_600_000_000_000_i64
        });

        let msg = ch.parse_mattermost_post(&post, "bot123", 1_500_000_000_000_i64, "chan789");
        assert!(msg.is_none());
    }

    #[test]
    fn mattermost_parse_post_ignore_old() {
        let ch = MattermostChannel::new("url".into(), "token".into(), None, vec!["*".into()]);
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "old message",
            "create_at": 1_400_000_000_000_i64
        });

        let msg = ch.parse_mattermost_post(&post, "bot123", 1_500_000_000_000_i64, "chan789");
        assert!(msg.is_none());
    }
}
