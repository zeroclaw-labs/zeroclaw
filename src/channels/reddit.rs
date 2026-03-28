use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::{Result, bail};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Deserialize;
use std::time::{Duration, Instant};

/// Reddit channel — polls for mentions, DMs, and comment replies via Reddit OAuth2 API.
pub struct RedditChannel {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    username: String,
    subreddit: Option<String>,
    auth: Mutex<RedditAuth>,
}

struct RedditAuth {
    access_token: String,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct RedditTokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct RedditListing {
    data: RedditListingData,
}

#[derive(Deserialize)]
struct RedditListingData {
    children: Vec<RedditChild>,
}

#[derive(Deserialize)]
struct RedditChild {
    data: RedditItemData,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct RedditItemData {
    name: Option<String>,
    author: Option<String>,
    body: Option<String>,
    subject: Option<String>,
    parent_id: Option<String>,
    link_id: Option<String>,
    subreddit: Option<String>,
    created_utc: Option<f64>,
    new: Option<bool>,
    #[serde(rename = "type")]
    message_type: Option<String>,
    context: Option<String>,
}

const REDDIT_API_BASE: &str = "https://oauth.reddit.com";
const REDDIT_TOKEN_URL: &str = "https://www.reddit.com/api/v1/access_token";
const USER_AGENT: &str = "zeroclaw:channel:v0.1.0 (by /u/zeroclaw-bot)";
/// Reddit enforces 60 requests per minute.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

impl RedditChannel {
    pub fn new(
        client_id: String,
        client_secret: String,
        refresh_token: String,
        username: String,
        subreddit: Option<String>,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            refresh_token,
            username,
            subreddit,
            auth: Mutex::new(RedditAuth {
                access_token: String::new(),
                expires_at: Instant::now(),
            }),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.reddit")
    }

    /// Refresh the OAuth2 access token using the refresh token.
    async fn refresh_access_token(&self) -> Result<()> {
        let client = self.http_client();
        let resp = client
            .post(REDDIT_TOKEN_URL)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .header("User-Agent", USER_AGENT)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.refresh_token),
            ])
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("Reddit token refresh failed ({status}): {body}");
        }

        let token_resp: RedditTokenResponse = resp.json().await?;
        let mut auth = self.auth.lock();
        auth.access_token = token_resp.access_token;
        auth.expires_at =
            Instant::now() + Duration::from_secs(token_resp.expires_in.saturating_sub(60));
        Ok(())
    }

    /// Get a valid access token, refreshing if expired.
    async fn get_access_token(&self) -> Result<String> {
        {
            let auth = self.auth.lock();
            if !auth.access_token.is_empty() && Instant::now() < auth.expires_at {
                return Ok(auth.access_token.clone());
            }
        }
        self.refresh_access_token().await?;
        let auth = self.auth.lock();
        Ok(auth.access_token.clone())
    }

    /// Fetch unread inbox items (mentions, DMs, comment replies).
    async fn fetch_inbox(&self) -> Result<Vec<RedditChild>> {
        let token = self.get_access_token().await?;
        let client = self.http_client();

        let resp = client
            .get(format!("{REDDIT_API_BASE}/message/unread"))
            .bearer_auth(&token)
            .header("User-Agent", USER_AGENT)
            .query(&[("limit", "25")])
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            tracing::warn!("Reddit inbox fetch failed ({status}): {body}");
            return Ok(Vec::new());
        }

        let listing: RedditListing = resp.json().await?;
        Ok(listing.data.children)
    }

    /// Mark inbox items as read.
    async fn mark_read(&self, fullnames: &[String]) -> Result<()> {
        if fullnames.is_empty() {
            return Ok(());
        }
        let token = self.get_access_token().await?;
        let client = self.http_client();

        let ids = fullnames.join(",");
        let resp = client
            .post(format!("{REDDIT_API_BASE}/api/read_message"))
            .bearer_auth(&token)
            .header("User-Agent", USER_AGENT)
            .form(&[("id", ids.as_str())])
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::warn!("Reddit mark_read failed: {}", resp.status());
        }
        Ok(())
    }

    /// Parse a Reddit inbox item into a ChannelMessage.
    fn parse_item(&self, item: &RedditItemData) -> Option<ChannelMessage> {
        let author = item.author.as_deref().unwrap_or("");
        let body = item.body.as_deref().unwrap_or("");
        let name = item.name.as_deref().unwrap_or("");

        // Skip messages from ourselves
        if author.eq_ignore_ascii_case(&self.username) || author.is_empty() || body.is_empty() {
            return None;
        }

        // If a subreddit filter is set, skip items from other subreddits
        if let Some(ref sub) = self.subreddit {
            if let Some(ref item_sub) = item.subreddit {
                if !item_sub.eq_ignore_ascii_case(sub) {
                    return None;
                }
            }
        }

        // Determine reply target: for comment replies use the parent thing name,
        // for DMs reply to the author.
        let reply_target =
            if item.message_type.as_deref() == Some("comment_reply") || item.parent_id.is_some() {
                // For comment replies, the recipient is the parent fullname
                item.parent_id.clone().unwrap_or_else(|| name.to_string())
            } else {
                // For DMs, reply to the author
                author.to_string()
            };

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let timestamp = item.created_utc.unwrap_or(0.0) as u64;

        Some(ChannelMessage {
            id: format!("reddit_{name}"),
            sender: author.to_string(),
            reply_target,
            content: body.to_string(),
            channel: "reddit".to_string(),
            timestamp,
            thread_ts: item.parent_id.clone(),
            interruption_scope_id: None,
            attachments: vec![],
            observe_group: false,
        })
    }
}

#[async_trait]
impl Channel for RedditChannel {
    fn name(&self) -> &str {
        "reddit"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let token = self.get_access_token().await?;
        let client = self.http_client();

        // If recipient looks like a Reddit fullname (t1_, t3_, t4_), it's a comment reply.
        // Otherwise treat it as a DM to a username.
        if message.recipient.starts_with("t1_")
            || message.recipient.starts_with("t3_")
            || message.recipient.starts_with("t4_")
        {
            // Comment reply
            let resp = client
                .post(format!("{REDDIT_API_BASE}/api/comment"))
                .bearer_auth(&token)
                .header("User-Agent", USER_AGENT)
                .form(&[
                    ("thing_id", message.recipient.as_str()),
                    ("text", &message.content),
                ])
                .send()
                .await?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
                bail!("Reddit comment reply failed ({status}): {body}");
            }
        } else {
            // Direct message
            let subject = message
                .subject
                .as_deref()
                .unwrap_or("Message from ZeroClaw");
            let resp = client
                .post(format!("{REDDIT_API_BASE}/api/compose"))
                .bearer_auth(&token)
                .header("User-Agent", USER_AGENT)
                .form(&[
                    ("to", message.recipient.as_str()),
                    ("subject", subject),
                    ("text", &message.content),
                ])
                .send()
                .await?;

            let status = resp.status();
            if !status.is_success() {
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
                bail!("Reddit DM failed ({status}): {body}");
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Initial auth
        self.refresh_access_token().await?;

        tracing::info!(
            "Reddit channel listening as u/{} {}...",
            self.username,
            self.subreddit
                .as_ref()
                .map(|s| format!("in r/{s}"))
                .unwrap_or_default()
        );

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let items = match self.fetch_inbox().await {
                Ok(items) => items,
                Err(e) => {
                    tracing::warn!("Reddit poll error: {e}");
                    continue;
                }
            };

            let mut read_ids = Vec::new();
            for child in &items {
                if let Some(ref name) = child.data.name {
                    read_ids.push(name.clone());
                }
                if let Some(msg) = self.parse_item(&child.data) {
                    if tx.send(msg).await.is_err() {
                        return Ok(());
                    }
                }
            }

            if let Err(e) = self.mark_read(&read_ids).await {
                tracing::warn!("Reddit mark_read error: {e}");
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.get_access_token().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> RedditChannel {
        RedditChannel::new(
            "client_id".into(),
            "client_secret".into(),
            "refresh_token".into(),
            "testbot".into(),
            None,
        )
    }

    fn make_channel_with_sub(sub: &str) -> RedditChannel {
        RedditChannel::new(
            "client_id".into(),
            "client_secret".into(),
            "refresh_token".into(),
            "testbot".into(),
            Some(sub.into()),
        )
    }

    #[test]
    fn parse_comment_reply() {
        let ch = make_channel();
        let item = RedditItemData {
            name: Some("t1_abc123".into()),
            author: Some("user1".into()),
            body: Some("hello bot".into()),
            subject: None,
            parent_id: Some("t1_parent1".into()),
            link_id: Some("t3_post1".into()),
            subreddit: Some("rust".into()),
            created_utc: Some(1_700_000_000.0),
            new: Some(true),
            message_type: Some("comment_reply".into()),
            context: None,
        };

        let msg = ch.parse_item(&item).unwrap();
        assert_eq!(msg.sender, "user1");
        assert_eq!(msg.content, "hello bot");
        assert_eq!(msg.reply_target, "t1_parent1");
        assert_eq!(msg.channel, "reddit");
        assert_eq!(msg.id, "reddit_t1_abc123");
    }

    #[test]
    fn parse_dm() {
        let ch = make_channel();
        let item = RedditItemData {
            name: Some("t4_dm456".into()),
            author: Some("user2".into()),
            body: Some("private message".into()),
            subject: Some("Hello".into()),
            parent_id: None,
            link_id: None,
            subreddit: None,
            created_utc: Some(1_700_000_100.0),
            new: Some(true),
            message_type: None,
            context: None,
        };

        let msg = ch.parse_item(&item).unwrap();
        assert_eq!(msg.sender, "user2");
        assert_eq!(msg.content, "private message");
        assert_eq!(msg.reply_target, "user2"); // DM reply goes to author
    }

    #[test]
    fn skip_self_messages() {
        let ch = make_channel();
        let item = RedditItemData {
            name: Some("t1_self".into()),
            author: Some("testbot".into()),
            body: Some("my own message".into()),
            subject: None,
            parent_id: None,
            link_id: None,
            subreddit: None,
            created_utc: Some(1_700_000_000.0),
            new: Some(true),
            message_type: None,
            context: None,
        };

        assert!(ch.parse_item(&item).is_none());
    }

    #[test]
    fn skip_empty_body() {
        let ch = make_channel();
        let item = RedditItemData {
            name: Some("t1_empty".into()),
            author: Some("user1".into()),
            body: Some(String::new()),
            subject: None,
            parent_id: None,
            link_id: None,
            subreddit: None,
            created_utc: Some(1_700_000_000.0),
            new: Some(true),
            message_type: None,
            context: None,
        };

        assert!(ch.parse_item(&item).is_none());
    }

    #[test]
    fn subreddit_filter() {
        let ch = make_channel_with_sub("rust");
        let item = RedditItemData {
            name: Some("t1_other".into()),
            author: Some("user1".into()),
            body: Some("hello".into()),
            subject: None,
            parent_id: None,
            link_id: None,
            subreddit: Some("python".into()),
            created_utc: Some(1_700_000_000.0),
            new: Some(true),
            message_type: None,
            context: None,
        };

        assert!(ch.parse_item(&item).is_none());

        let matching_item = RedditItemData {
            name: Some("t1_match".into()),
            author: Some("user1".into()),
            body: Some("hello".into()),
            subject: None,
            parent_id: None,
            link_id: None,
            subreddit: Some("rust".into()),
            created_utc: Some(1_700_000_000.0),
            new: Some(true),
            message_type: None,
            context: None,
        };

        assert!(ch.parse_item(&matching_item).is_some());
    }

    #[test]
    fn send_message_formatting() {
        // Verify SendMessage can be constructed for both DM and comment reply
        let dm = SendMessage::new("hello", "user1");
        assert_eq!(dm.recipient, "user1");
        assert_eq!(dm.content, "hello");

        let reply = SendMessage::new("response", "t1_abc123");
        assert!(reply.recipient.starts_with("t1_"));
    }
}
