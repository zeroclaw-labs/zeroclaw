use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::Result;
use async_trait::async_trait;
use portable_atomic::{AtomicU64, Ordering};
use serde::Deserialize;

/// Default Mastodon character limit per status.
const DEFAULT_CHAR_LIMIT: usize = 500;

/// Polling interval when streaming is unavailable or between poll cycles.
const POLL_INTERVAL_SECS: u64 = 10;

/// Monotonic counter for unique message IDs.
static MSG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Mastodon visibility levels for posted statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Visible to everyone, appears on public timelines.
    Public,
    /// Visible to everyone, but does not appear on public timelines.
    Unlisted,
    /// Visible only to followers.
    Private,
    /// Visible only to mentioned users (direct message).
    Direct,
}

impl Visibility {
    fn as_api_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Unlisted => "unlisted",
            Self::Private => "private",
            Self::Direct => "direct",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "public" => Self::Public,
            "private" => Self::Private,
            "direct" => Self::Direct,
            // "unlisted" and any unrecognized value default to unlisted
            _ => Self::Unlisted,
        }
    }
}

/// Mastodon channel — connects to a Mastodon instance via its REST API.
///
/// Listens for mentions by polling the notifications endpoint and posts
/// status replies using the statuses API. Supports content warnings,
/// configurable visibility levels, and respects per-instance character limits.
pub struct MastodonChannel {
    instance_url: String,
    access_token: String,
    default_visibility: Visibility,
    char_limit: usize,
    spoiler_text: Option<String>,
}

/// Configuration for constructing a `MastodonChannel`.
pub struct MastodonChannelConfig {
    pub instance_url: String,
    pub access_token: String,
    pub default_visibility: Option<String>,
    pub char_limit: Option<usize>,
    pub spoiler_text: Option<String>,
}

impl MastodonChannel {
    pub fn new(cfg: MastodonChannelConfig) -> Self {
        let instance_url = cfg.instance_url.trim_end_matches('/').to_string();
        let default_visibility = cfg
            .default_visibility
            .as_deref()
            .map(Visibility::from_str)
            .unwrap_or(Visibility::Unlisted);
        Self {
            instance_url,
            access_token: cfg.access_token,
            default_visibility,
            char_limit: cfg.char_limit.unwrap_or(DEFAULT_CHAR_LIMIT),
            spoiler_text: cfg.spoiler_text,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.mastodon")
    }

    /// Split a message into chunks that fit within the configured character limit.
    fn split_status(text: &str, limit: usize) -> Vec<String> {
        if limit == 0 || text.len() <= limit {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut remaining = text;

        while !remaining.is_empty() {
            if remaining.len() <= limit {
                chunks.push(remaining.to_string());
                break;
            }

            // Try to split at a whitespace boundary
            let split_at = remaining[..limit]
                .rfind(char::is_whitespace)
                .unwrap_or(limit);
            let split_at = if split_at == 0 { limit } else { split_at };

            // Ensure we split at a valid char boundary
            let split_at = {
                let mut pos = split_at;
                while pos > 0 && !remaining.is_char_boundary(pos) {
                    pos -= 1;
                }
                if pos == 0 {
                    // Advance forward
                    pos = split_at;
                    while pos < remaining.len() && !remaining.is_char_boundary(pos) {
                        pos += 1;
                    }
                }
                pos
            };

            chunks.push(remaining[..split_at].to_string());
            remaining = remaining[split_at..].trim_start();
        }

        if chunks.is_empty() {
            chunks.push(String::new());
        }

        chunks
    }

    /// Post a status to the Mastodon instance.
    async fn post_status(
        &self,
        text: &str,
        in_reply_to_id: Option<&str>,
        visibility: Visibility,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/api/v1/statuses", self.instance_url);
        let mut form = vec![
            ("status".to_string(), text.to_string()),
            (
                "visibility".to_string(),
                visibility.as_api_str().to_string(),
            ),
        ];

        if let Some(ref cw) = self.spoiler_text {
            form.push(("spoiler_text".to_string(), cw.clone()));
        }

        if let Some(reply_id) = in_reply_to_id {
            form.push(("in_reply_to_id".to_string(), reply_id.to_string()));
        }

        let resp = self
            .http_client()
            .post(&url)
            .bearer_auth(&self.access_token)
            .form(&form)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Mastodon post_status failed ({status}): {body}");
        }

        let json: serde_json::Value = resp.json().await?;
        Ok(json)
    }

    /// Fetch recent mention notifications.
    async fn poll_mentions(&self, since_id: Option<&str>) -> Result<Vec<MastodonNotification>> {
        let mut url = format!(
            "{}/api/v1/notifications?types[]=mention&limit=30",
            self.instance_url
        );
        if let Some(id) = since_id {
            use std::fmt::Write;
            let _ = write!(url, "&since_id={id}");
        }

        let resp = self
            .http_client()
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Mastodon poll_mentions failed ({status}): {body}");
        }

        let notifications: Vec<MastodonNotification> = resp.json().await?;
        Ok(notifications)
    }
}

/// A Mastodon notification (subset of fields).
#[derive(Debug, Clone, Deserialize)]
struct MastodonNotification {
    id: String,
    #[serde(rename = "type")]
    notification_type: String,
    account: MastodonAccount,
    status: Option<MastodonStatus>,
}

/// A Mastodon account (subset of fields).
#[derive(Debug, Clone, Deserialize)]
struct MastodonAccount {
    #[serde(default)]
    acct: String,
}

/// A Mastodon status (subset of fields).
#[derive(Debug, Clone, Deserialize)]
struct MastodonStatus {
    id: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    visibility: String,
}

/// Strip HTML tags from Mastodon status content (basic implementation).
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                // <br> and </p> become newlines
            }
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

#[async_trait]
impl Channel for MastodonChannel {
    fn name(&self) -> &str {
        "mastodon"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // recipient holds the status ID to reply to (if any)
        let reply_to = if message.recipient.is_empty() {
            None
        } else {
            Some(message.recipient.as_str())
        };

        let chunks = Self::split_status(&message.content, self.char_limit);
        let mut last_id = reply_to.map(String::from);

        for chunk in &chunks {
            let resp = self
                .post_status(chunk, last_id.as_deref(), self.default_visibility)
                .await?;
            // Thread subsequent chunks as replies to the previous one
            if let Some(id) = resp.get("id").and_then(|v| v.as_str()) {
                last_id = Some(id.to_string());
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!(
            "Mastodon channel polling mentions on {}...",
            self.instance_url
        );

        let mut last_seen_id: Option<String> = None;

        loop {
            match self.poll_mentions(last_seen_id.as_deref()).await {
                Ok(notifications) => {
                    // Process oldest-first
                    for notif in notifications.iter().rev() {
                        if notif.notification_type != "mention" {
                            continue;
                        }
                        let Some(ref status) = notif.status else {
                            continue;
                        };

                        let plain_text = strip_html_tags(&status.content);
                        let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);

                        let channel_msg = ChannelMessage {
                            id: format!("mastodon_{seq}_{}", status.id),
                            sender: notif.account.acct.clone(),
                            reply_target: status.id.clone(),
                            content: plain_text,
                            channel: "mastodon".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            thread_ts: Some(status.id.clone()),
                        };

                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }

                    // Update since_id to the newest notification
                    if let Some(newest) = notifications.first() {
                        last_seen_id = Some(newest.id.clone());
                    }
                }
                Err(e) => {
                    tracing::warn!("Mastodon poll error: {e:#}");
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/api/v1/accounts/verify_credentials", self.instance_url);
        let resp = self
            .http_client()
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await;
        resp.is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Config / constructor ──────────────────────────────────

    #[test]
    fn config_defaults_visibility_to_unlisted() {
        let ch = MastodonChannel::new(MastodonChannelConfig {
            instance_url: "https://mastodon.social".into(),
            access_token: "token123".into(),
            default_visibility: None,
            char_limit: None,
            spoiler_text: None,
        });
        assert_eq!(ch.default_visibility, Visibility::Unlisted);
        assert_eq!(ch.char_limit, DEFAULT_CHAR_LIMIT);
    }

    #[test]
    fn config_accepts_explicit_visibility() {
        let ch = MastodonChannel::new(MastodonChannelConfig {
            instance_url: "https://mastodon.social".into(),
            access_token: "token123".into(),
            default_visibility: Some("direct".into()),
            char_limit: Some(1000),
            spoiler_text: Some("cw".into()),
        });
        assert_eq!(ch.default_visibility, Visibility::Direct);
        assert_eq!(ch.char_limit, 1000);
        assert_eq!(ch.spoiler_text.as_deref(), Some("cw"));
    }

    #[test]
    fn config_strips_trailing_slash_from_instance_url() {
        let ch = MastodonChannel::new(MastodonChannelConfig {
            instance_url: "https://mastodon.social/".into(),
            access_token: "tok".into(),
            default_visibility: None,
            char_limit: None,
            spoiler_text: None,
        });
        assert_eq!(ch.instance_url, "https://mastodon.social");
    }

    #[test]
    fn name_returns_mastodon() {
        let ch = make_channel();
        assert_eq!(ch.name(), "mastodon");
    }

    // ── Visibility ────────────────────────────────────────────

    #[test]
    fn visibility_roundtrip() {
        for &vis in &[
            Visibility::Public,
            Visibility::Unlisted,
            Visibility::Private,
            Visibility::Direct,
        ] {
            let parsed = Visibility::from_str(vis.as_api_str());
            assert_eq!(parsed, vis);
        }
    }

    #[test]
    fn visibility_unknown_defaults_to_unlisted() {
        assert_eq!(Visibility::from_str("bogus"), Visibility::Unlisted);
    }

    // ── Status splitting ──────────────────────────────────────

    #[test]
    fn split_short_status_returns_single_chunk() {
        let chunks = MastodonChannel::split_status("Hello world", 500);
        assert_eq!(chunks, vec!["Hello world"]);
    }

    #[test]
    fn split_long_status_respects_limit() {
        let text = "word ".repeat(200); // ~1000 chars
        let chunks = MastodonChannel::split_status(text.trim(), 500);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 500);
        }
    }

    #[test]
    fn split_status_at_word_boundary() {
        let text = "aaaa bbbb cccc";
        let chunks = MastodonChannel::split_status(text, 9);
        // Should split at whitespace: "aaaa" and "bbbb cccc" or similar
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 9);
        }
    }

    #[test]
    fn split_empty_status() {
        let chunks = MastodonChannel::split_status("", 500);
        assert_eq!(chunks, vec![""]);
    }

    // ── HTML stripping ────────────────────────────────────────

    #[test]
    fn strip_html_removes_tags() {
        let html = "<p>Hello <strong>world</strong></p>";
        assert_eq!(strip_html_tags(html), "Hello world");
    }

    #[test]
    fn strip_html_decodes_entities() {
        let html = "A &amp; B &lt; C &gt; D &quot;E&quot;";
        assert_eq!(strip_html_tags(html), "A & B < C > D \"E\"");
    }

    #[test]
    fn strip_html_plain_text_unchanged() {
        assert_eq!(strip_html_tags("plain text"), "plain text");
    }

    // ── Config serde ──────────────────────────────────────────

    #[test]
    fn mastodon_config_serde_roundtrip() {
        use crate::config::schema::MastodonConfig;

        let config = MastodonConfig {
            instance_url: "https://mastodon.social".into(),
            access_token: "tok_abc123".into(),
            default_visibility: Some("direct".into()),
            char_limit: Some(1000),
            spoiler_text: Some("content warning".into()),
        };

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: MastodonConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.instance_url, "https://mastodon.social");
        assert_eq!(parsed.access_token, "tok_abc123");
        assert_eq!(parsed.default_visibility.as_deref(), Some("direct"));
        assert_eq!(parsed.char_limit, Some(1000));
        assert_eq!(parsed.spoiler_text.as_deref(), Some("content warning"));
    }

    #[test]
    fn mastodon_config_minimal_toml() {
        use crate::config::schema::MastodonConfig;

        let toml_str = r#"
instance_url = "https://mastodon.social"
access_token = "tok_test"
"#;
        let parsed: MastodonConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.instance_url, "https://mastodon.social");
        assert_eq!(parsed.access_token, "tok_test");
        assert!(parsed.default_visibility.is_none());
        assert!(parsed.char_limit.is_none());
        assert!(parsed.spoiler_text.is_none());
    }

    // ── Helpers ───────────────────────────────────────────────

    fn make_channel() -> MastodonChannel {
        MastodonChannel::new(MastodonChannelConfig {
            instance_url: "https://mastodon.social".into(),
            access_token: "test_token".into(),
            default_visibility: None,
            char_limit: None,
            spoiler_text: None,
        })
    }
}
