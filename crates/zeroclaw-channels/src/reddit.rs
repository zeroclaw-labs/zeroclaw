use anyhow::{Result, bail};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// Reddit channel — polls for mentions, DMs, and comment replies via Reddit OAuth2 API.
pub struct RedditChannel {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    username: String,
    /// Empty = accept items from any subreddit the bot has access to.
    /// This is a source filter layered on top of sender authorization, never
    /// a substitute for it.
    subreddits: Vec<String>,
    /// The alias key under `[channels.reddit.<alias>]` this handle is
    /// bound to. Used for attribution and peer-group resolution.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// The resolver reads live configuration and is not cached.
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
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
        alias: impl Into<String>,
        client_id: String,
        client_secret: String,
        refresh_token: String,
        username: String,
        subreddits: Vec<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            refresh_token,
            username,
            subreddits,
            alias: alias.into(),
            peer_resolver,
            auth: Mutex::new(RedditAuth {
                access_token: String::new(),
                expires_at: Instant::now(),
            }),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.reddit")
    }

    /// Strip the `u/` prefix an operator is likely to paste, so a peer entry
    /// written the way Reddit displays it still names the account.
    fn normalize_identity(value: &str) -> &str {
        let v = value.trim().trim_start_matches('@');
        v.strip_prefix("/u/")
            .or_else(|| v.strip_prefix("u/"))
            .unwrap_or(v)
    }

    /// Reddit usernames are unique case-insensitively, and this file already
    /// compares the bot's own name that way, so a peer entry differing only in
    /// case names the same account and matching it admits no one new.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed_by(&peers, user_id, |entry, user| {
            Self::normalize_identity(entry).eq_ignore_ascii_case(Self::normalize_identity(user))
        })
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
            bail!("token refresh failed ({status}): {body}");
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"status": status.to_string(), "body": body})),
                "inbox fetch failed"
            );
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("mark_read failed: {}", resp.status())
            );
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

        // The inbox mixes mentions, comment replies and DMs, and none of them
        // are consent to be driven. An empty peer group denies everyone; `"*"`
        // is the explicit opt-in for a public bot.
        if !self.is_user_allowed(author) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"author": author})),
                "ignoring inbox item from unauthorized sender"
            );
            return None;
        }

        // If a subreddit allowlist is set, skip items from other subreddits.
        // Items without a subreddit (e.g. DMs) carry no subreddit to filter on
        // and are already covered by the sender check above.
        if !self.subreddits.is_empty()
            && let Some(ref item_sub) = item.subreddit
            && !self
                .subreddits
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(item_sub))
        {
            return None;
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
            channel_alias: None,
            timestamp,
            thread_ts: item.parent_id.clone(),
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
    }
}

impl ::zeroclaw_api::attribution::Attributable for RedditChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Reddit)
    }
    fn alias(&self) -> &str {
        &self.alias
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
                bail!("comment reply failed ({status}): {body}");
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
                bail!("DM failed ({status}): {body}");
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Initial auth
        self.refresh_access_token().await?;

        let scope = if self.subreddits.is_empty() {
            String::new()
        } else {
            format!(
                "in {}",
                self.subreddits
                    .iter()
                    .map(|s| format!("r/{s}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("channel listening as u/{} {}...", self.username, scope)
        );

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let items = match self.fetch_inbox().await {
                Ok(items) => items,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "poll error"
                    );
                    continue;
                }
            };

            let mut read_ids = Vec::new();
            for child in &items {
                if let Some(ref name) = child.data.name {
                    read_ids.push(name.clone());
                }
                if let Some(msg) = self.parse_item(&child.data)
                    && tx.send(msg).await.is_err()
                {
                    return Ok(());
                }
            }

            if let Err(e) = self.mark_read(&read_ids).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "mark_read error"
                );
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.get_access_token().await.is_ok()
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing-indicator endpoint in the Reddit API.
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The senders the shared fixtures use. Tests that care about the
    /// authorization boundary itself build their own peer set.
    fn default_peers() -> Vec<String> {
        vec!["user1".to_string(), "user2".to_string()]
    }

    fn make_channel() -> RedditChannel {
        make_channel_with(Vec::new(), default_peers())
    }

    fn make_channel_with_sub(sub: &str) -> RedditChannel {
        make_channel_with(vec![sub.into()], default_peers())
    }

    fn make_channel_with(subreddits: Vec<String>, peers: Vec<String>) -> RedditChannel {
        RedditChannel::new(
            "testbot",
            "client_id".into(),
            "client_secret".into(),
            "refresh_token".into(),
            "testbot".into(),
            subreddits,
            Arc::new(move || peers.clone()),
        )
    }

    /// An inbox item from `author`, shaped as a DM (no subreddit, no parent).
    fn dm_from(author: &str) -> RedditItemData {
        RedditItemData {
            name: Some("t4_dm".into()),
            author: Some(author.into()),
            body: Some("private message".into()),
            subject: Some("Hello".into()),
            parent_id: None,
            link_id: None,
            subreddit: None,
            created_utc: Some(1_700_000_100.0),
            new: Some(true),
            message_type: None,
            context: None,
        }
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
    fn drops_comment_reply_from_unauthorized_sender() {
        let ch = make_channel();
        let item = RedditItemData {
            name: Some("t1_abc123".into()),
            author: Some("intruder".into()),
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

        assert!(ch.parse_item(&item).is_none());
    }

    #[test]
    fn drops_dm_from_unauthorized_sender() {
        // The previous behaviour accepted any DM outright, because a DM has no
        // subreddit for the only filter on the path to act on.
        let ch = make_channel();

        assert!(ch.parse_item(&dm_from("intruder")).is_none());
    }

    #[test]
    fn empty_peer_group_denies_everyone() {
        let ch = make_channel_with(Vec::new(), Vec::new());

        assert!(ch.parse_item(&dm_from("user1")).is_none());
    }

    #[test]
    fn wildcard_peer_allows_a_public_bot() {
        let ch = make_channel_with(Vec::new(), vec!["*".to_string()]);

        let msg = ch.parse_item(&dm_from("anyone")).unwrap();
        assert_eq!(msg.sender, "anyone");
    }

    #[test]
    fn ignored_peer_is_denied_under_a_wildcard_grant() {
        // The list shape `Config::channel_external_peers` produces when one
        // matching group grants `["*"]` and another ignores a sender.
        let ch = make_channel_with(Vec::new(), vec!["*".to_string(), "!alice".to_string()]);

        assert!(
            ch.parse_item(&dm_from("alice")).is_none(),
            "an ignored sender must not ride the wildcard"
        );
        assert_eq!(
            ch.parse_item(&dm_from("bob"))
                .expect("an unignored sender still rides the wildcard")
                .sender,
            "bob"
        );
    }

    /// Resolve peers the way the daemon does, from config, so the test covers
    /// the config-to-adapter boundary rather than a hand-built vector.
    fn peers_from_config(toml_src: &str, alias: &str) -> Vec<String> {
        let config: zeroclaw_config::schema::Config =
            toml::from_str(toml_src).expect("peer-group config should parse");

        config.channel_external_peers("reddit", alias)
    }

    #[test]
    fn ignore_denies_a_grant_written_with_the_display_prefix() {
        // Reddit reads `u/alice` and `alice` as one account. Resolving the deny
        // by comparing raw strings kept the grant and dropped the `ignore`, and
        // this gate then authorized the sender the operator had ignored.
        let ch = make_channel_with(
            Vec::new(),
            peers_from_config(
                r#"
                [peer_groups.reddit_ops]
                channel = "reddit.ops"
                external_peers = ["u/alice"]
                ignore = ["alice"]
                "#,
                "ops",
            ),
        );

        assert!(
            ch.parse_item(&dm_from("alice")).is_none(),
            "a deny naming the account must survive the resolver"
        );
    }

    #[test]
    fn ignore_written_with_the_display_prefix_denies_a_bare_grant() {
        // The inverse spelling, so neither side of the comparison is privileged.
        let ch = make_channel_with(
            Vec::new(),
            peers_from_config(
                r#"
                [peer_groups.reddit_ops]
                channel = "reddit.ops"
                external_peers = ["alice"]
                ignore = ["u/alice"]
                "#,
                "ops",
            ),
        );

        assert!(ch.parse_item(&dm_from("alice")).is_none());
    }

    #[test]
    fn config_wildcard_with_ignore_denies_only_the_ignored_sender() {
        let peers = peers_from_config(
            r#"
            [peer_groups.reddit_all]
            channel = "reddit"
            external_peers = ["*"]

            [peer_groups.reddit_ops]
            channel = "reddit.ops"
            ignore = ["alice"]
            "#,
            "ops",
        );
        let ch = make_channel_with(Vec::new(), peers);

        assert!(ch.parse_item(&dm_from("alice")).is_none());
        assert_eq!(
            ch.parse_item(&dm_from("bob"))
                .expect("an unignored sender still rides the wildcard")
                .sender,
            "bob"
        );
    }

    #[test]
    fn config_ignoring_the_wildcard_denies_everyone() {
        let ch = make_channel_with(
            Vec::new(),
            peers_from_config(
                r#"
                [peer_groups.reddit_all]
                channel = "reddit"
                external_peers = ["*"]
                ignore = ["*"]
                "#,
                "ops",
            ),
        );

        assert!(ch.parse_item(&dm_from("alice")).is_none());
        assert!(ch.parse_item(&dm_from("bob")).is_none());
    }

    #[test]
    fn ignored_peer_is_denied_regardless_of_username_case() {
        // Reddit usernames are unique case-insensitively, so a deny written in
        // one case has to cover the account in any case.
        let ch = make_channel_with(Vec::new(), vec!["*".to_string(), "!Alice".to_string()]);

        assert!(ch.parse_item(&dm_from("alice")).is_none());
        assert!(ch.parse_item(&dm_from("ALICE")).is_none());
    }

    #[test]
    fn peer_match_ignores_username_case() {
        // Reddit usernames are unique case-insensitively, so `User1` and
        // `user1` are the same account rather than two.
        let ch = make_channel_with(Vec::new(), vec!["User1".to_string()]);

        let msg = ch.parse_item(&dm_from("user1")).unwrap();
        assert_eq!(msg.sender, "user1");
    }

    #[test]
    fn subreddit_filter_does_not_substitute_for_sender_authorization() {
        // An allowed subreddit is not enough on its own: the sender still has
        // to be a peer.
        let ch = make_channel_with(vec!["rust".to_string()], default_peers());
        let mut item = dm_from("intruder");
        item.subreddit = Some("rust".into());

        assert!(ch.parse_item(&item).is_none());

        let mut allowed = dm_from("user1");
        allowed.subreddit = Some("rust".into());
        assert!(ch.parse_item(&allowed).is_some());
    }

    #[test]
    fn authorized_sender_still_filtered_by_subreddit() {
        // And the converse: being a peer does not bypass the source filter.
        let ch = make_channel_with(vec!["rust".to_string()], default_peers());
        let mut item = dm_from("user1");
        item.subreddit = Some("python".into());

        assert!(ch.parse_item(&item).is_none());
    }

    #[test]
    fn peer_entry_may_carry_a_u_prefix() {
        // An operator pasting the name as Reddit displays it should not be
        // silently denied.
        for entry in ["u/user1", "/u/user1", "@user1", " user1 "] {
            let ch = make_channel_with(Vec::new(), vec![entry.to_string()]);
            assert!(
                ch.parse_item(&dm_from("user1")).is_some(),
                "peer entry {entry:?} should name u/user1"
            );
        }
    }

    #[test]
    fn prefix_normalization_does_not_admit_a_different_account() {
        let ch = make_channel_with(Vec::new(), vec!["u/user1".to_string()]);

        assert!(ch.parse_item(&dm_from("user2")).is_none());
        assert!(ch.parse_item(&dm_from("uuser1")).is_none());
    }

    #[test]
    fn peers_are_resolved_per_message_not_cached() {
        let peers = Arc::new(Mutex::new(Vec::<String>::new()));
        let ch = {
            let peers = peers.clone();
            RedditChannel::new(
                "testbot",
                "client_id".into(),
                "client_secret".into(),
                "refresh_token".into(),
                "testbot".into(),
                Vec::new(),
                Arc::new(move || peers.lock().clone()),
            )
        };

        assert!(ch.parse_item(&dm_from("user1")).is_none());
        peers.lock().push("user1".to_string());
        assert!(ch.parse_item(&dm_from("user1")).is_some());
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
