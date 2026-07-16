use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const TWITTER_API_BASE: &str = "https://api.x.com/2";

/// X/Twitter channel — uses the Twitter API v2 with OAuth 2.0 Bearer Token
/// for sending tweets/DMs and filtered stream for receiving mentions.
pub struct TwitterChannel {
    bearer_token: String,
    /// The alias key under `[channels.twitter.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Message deduplication set.
    dedup: Arc<RwLock<HashSet<String>>>,
}

/// Deduplication set capacity — evict half of entries when full.
const DEDUP_CAPACITY: usize = 10_000;

impl TwitterChannel {
    pub fn new(
        bearer_token: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            bearer_token,
            alias: alias.into(),
            peer_resolver,
            dedup: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Return the alias under `[channels.twitter.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.twitter")
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
    }

    /// Check and insert tweet ID for deduplication.
    async fn is_duplicate(&self, tweet_id: &str) -> bool {
        if tweet_id.is_empty() {
            return false;
        }

        let mut dedup = self.dedup.write().await;

        if dedup.contains(tweet_id) {
            return true;
        }

        if dedup.len() >= DEDUP_CAPACITY {
            let to_remove: Vec<String> = dedup.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
            for key in to_remove {
                dedup.remove(&key);
            }
        }

        dedup.insert(tweet_id.to_string());
        false
    }

    /// Get the authenticated user's ID for filtered stream rules.
    async fn get_authenticated_user_id(&self) -> anyhow::Result<String> {
        let resp = self
            .http_client()
            .get(format!("{TWITTER_API_BASE}/users/me"))
            .bearer_auth(&self.bearer_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Twitter users/me failed ({status}): {err}");
        }

        let data: serde_json::Value = resp.json().await?;
        let user_id = data
            .get("data")
            .and_then(|d| d.get("id"))
            .and_then(|id| id.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "Missing user id in Twitter response"
                );
                anyhow::Error::msg("Missing user id in Twitter response")
            })?
            .to_string();

        Ok(user_id)
    }

    /// Send a reply tweet.
    async fn create_tweet(
        &self,
        text: &str,
        reply_tweet_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let mut body = json!({ "text": text });

        if let Some(reply_id) = reply_tweet_id {
            body["reply"] = json!({ "in_reply_to_tweet_id": reply_id });
        }

        let resp = self
            .http_client()
            .post(format!("{TWITTER_API_BASE}/tweets"))
            .bearer_auth(&self.bearer_token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Twitter create tweet failed ({status}): {err}");
        }

        let data: serde_json::Value = resp.json().await?;
        let tweet_id = data
            .get("data")
            .and_then(|d| d.get("id"))
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_string();

        Ok(tweet_id)
    }

    /// Send a DM to a user.
    async fn send_dm(&self, recipient_id: &str, text: &str) -> anyhow::Result<()> {
        let body = json!({
            "text": text,
        });

        let resp = self
            .http_client()
            .post(format!(
                "{TWITTER_API_BASE}/dm_conversations/with/{recipient_id}/messages"
            ))
            .bearer_auth(&self.bearer_token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Twitter DM send failed ({status}): {err}");
        }

        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for TwitterChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Twitter,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for TwitterChannel {
    fn name(&self) -> &str {
        "twitter"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // recipient format: "dm:{user_id}" for DMs, "tweet:{tweet_id}" for replies
        if let Some(user_id) = message.recipient.strip_prefix("dm:") {
            // Twitter API enforces a 280 char limit on tweets but DMs can be up to 10000.
            self.send_dm(user_id, &message.content).await
        } else if let Some(tweet_id) = message.recipient.strip_prefix("tweet:") {
            // Split long replies into tweet threads (280 char limit).
            let chunks = split_tweet_text(&message.content, 280);
            let mut reply_to = tweet_id.to_string();
            for chunk in chunks {
                reply_to = self.create_tweet(&chunk, Some(&reply_to)).await?;
            }
            Ok(())
        } else {
            // Default: treat as tweet reply
            let chunks = split_tweet_text(&message.content, 280);
            let mut reply_to = message.recipient.clone();
            for chunk in chunks {
                reply_to = self.create_tweet(&chunk, Some(&reply_to)).await?;
            }
            Ok(())
        }
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "authenticating..."
        );
        let bot_user_id = self.get_authenticated_user_id().await?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"bot_user_id": bot_user_id})),
            "authenticated as user"
        );

        // Poll mentions timeline (filtered stream requires elevated access).
        // Using mentions timeline polling as a more accessible approach.
        let mut since_id: Option<String> = None;
        let poll_interval = std::time::Duration::from_secs(15);

        loop {
            let mut url = format!(
                "{TWITTER_API_BASE}/users/{bot_user_id}/mentions?tweet.fields=author_id,conversation_id,created_at&expansions=author_id&max_results=20"
            );

            if let Some(ref id) = since_id {
                use std::fmt::Write;
                let _ = write!(url, "&since_id={id}");
            }

            match self
                .http_client()
                .get(&url)
                .bearer_auth(&self.bearer_token)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let data: serde_json::Value = match resp.json().await {
                        Ok(d) => d,
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                "failed to parse mentions response"
                            );
                            tokio::time::sleep(poll_interval).await;
                            continue;
                        }
                    };

                    if let Some(tweets) = data.get("data").and_then(|d| d.as_array()) {
                        // Build user lookup map from includes
                        let user_map: std::collections::HashMap<String, String> = data
                            .get("includes")
                            .and_then(|i| i.get("users"))
                            .and_then(|u| u.as_array())
                            .map(|users| {
                                users
                                    .iter()
                                    .filter_map(|u| {
                                        let id = u.get("id")?.as_str()?.to_string();
                                        let username = u.get("username")?.as_str()?.to_string();
                                        Some((id, username))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();

                        // Process tweets in chronological order (oldest first)
                        for tweet in tweets.iter().rev() {
                            let tweet_id = tweet.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            let author_id = tweet
                                .get("author_id")
                                .and_then(|a| a.as_str())
                                .unwrap_or("");
                            let text = tweet.get("text").and_then(|t| t.as_str()).unwrap_or("");

                            // Skip own tweets
                            if author_id == bot_user_id {
                                continue;
                            }

                            if self.is_duplicate(tweet_id).await {
                                continue;
                            }

                            let username = user_map
                                .get(author_id)
                                .cloned()
                                .unwrap_or_else(|| author_id.to_string());

                            if !self.is_user_allowed(&username) && !self.is_user_allowed(author_id)
                            {
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_attrs(::serde_json::json!({"username": username})),
                                    "ignoring mention from unauthorized user"
                                );
                                continue;
                            }

                            let trimmed_text = text.trim();
                            if trimmed_text.is_empty() {
                                continue;
                            }

                            let reply_target = format!("tweet:{tweet_id}");

                            let channel_msg = ChannelMessage {
                                id: Uuid::new_v4().to_string(),
                                sender: username,
                                reply_target,
                                content: trimmed_text.to_string(),
                                channel: "twitter".to_string(),
                                channel_alias: Some(self.alias.clone()),
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                                thread_ts: tweet
                                    .get("conversation_id")
                                    .and_then(|c| c.as_str())
                                    .map(|s| s.to_string()),
                                interruption_scope_id: None,
                                attachments: vec![],
                                subject: None,

                                ..Default::default()
                            };

                            if tx.send(channel_msg).await.is_err() {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                    "message channel closed"
                                );
                                return Ok(());
                            }

                            // Track newest ID for pagination
                            if since_id.as_deref().is_none_or(|s| tweet_id > s) {
                                since_id = Some(tweet_id.to_string());
                            }
                        }
                    }

                    // Update newest_id from meta
                    if let Some(newest) = data
                        .get("meta")
                        .and_then(|m| m.get("newest_id"))
                        .and_then(|n| n.as_str())
                    {
                        since_id = Some(newest.to_string());
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    if status.as_u16() == 429 {
                        // Rate limited — back off
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            "rate limited, backing off 60s"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        continue;
                    }
                    let err = resp.text().await.unwrap_or_default();
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"error": format!("{}", err), "status": status.to_string()})), "mentions request failed");
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "mentions request error"
                    );
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn health_check(&self) -> bool {
        self.get_authenticated_user_id().await.is_ok()
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing-indicator endpoint in the Twitter/X v2 API.
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Split tweet text into tweet-sized chunks, breaking at word boundaries.
fn split_tweet_text(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        // Find last space within limit.
        let limit = crate::util::floor_char_boundary(remaining, max_len);
        let split_at = remaining[..limit].rfind(' ').unwrap_or(limit);

        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let ch = TwitterChannel::new("token".into(), "twitter_test_alias", Arc::new(Vec::new));
        assert_eq!(ch.name(), "twitter");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = TwitterChannel::new(
            "token".into(),
            "twitter_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = TwitterChannel::new(
            "token".into(),
            "twitter_test_alias",
            Arc::new(|| vec!["user123".into()]),
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = TwitterChannel::new("token".into(), "twitter_test_alias", Arc::new(Vec::new));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[tokio::test]
    async fn test_dedup() {
        let ch = TwitterChannel::new("token".into(), "twitter_test_alias", Arc::new(Vec::new));
        assert!(!ch.is_duplicate("tweet1").await);
        assert!(ch.is_duplicate("tweet1").await);
        assert!(!ch.is_duplicate("tweet2").await);
    }

    #[tokio::test]
    async fn test_dedup_empty_id() {
        let ch = TwitterChannel::new("token".into(), "twitter_test_alias", Arc::new(Vec::new));
        assert!(!ch.is_duplicate("").await);
        assert!(!ch.is_duplicate("").await);
    }

    #[test]
    fn test_split_tweet_text_short() {
        let chunks = split_tweet_text("hello", 280);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_tweet_text_long() {
        let text = "a ".repeat(200);
        let chunks = split_tweet_text(text.trim(), 280);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= 280);
        }
    }

    #[test]
    fn test_split_tweet_text_no_spaces() {
        let text = "a".repeat(300);
        let chunks = split_tweet_text(&text, 280);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 280);
    }

    #[test]
    fn test_split_tweet_text_safe_on_multibyte_boundary() {
        let text = format!("{}{}tail", "a".repeat(279), "😀");
        let chunks = split_tweet_text(&text, 280);

        assert_eq!(chunks.concat(), text);
        assert_eq!(chunks[0], "a".repeat(279));
        assert_eq!(chunks[1], "😀tail");
        for chunk in &chunks {
            assert!(chunk.is_char_boundary(chunk.len()));
        }
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
bearer_token = "AAAA"
"#;
        let config: zeroclaw_config::schema::TwitterConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bearer_token, "AAAA");
    }

    #[test]
    fn test_config_serde_defaults() {
        let toml_str = r#"
bearer_token = "tok"
"#;
        let config: zeroclaw_config::schema::TwitterConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bearer_token, "tok");
    }
}
