use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::{bail, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Bluesky channel — polls for mentions via AT Protocol and replies as posts.
pub struct BlueskyChannel {
    handle: String,
    app_password: String,
    auth: Mutex<BlueskyAuth>,
}

struct BlueskyAuth {
    access_jwt: String,
    refresh_jwt: String,
    did: String,
    expires_at: Instant,
}

const BSKY_API_BASE: &str = "https://bsky.social/xrpc";
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct CreateSessionResponse {
    #[serde(rename = "accessJwt")]
    access_jwt: String,
    #[serde(rename = "refreshJwt")]
    refresh_jwt: String,
    did: String,
}

#[derive(Deserialize)]
struct RefreshSessionResponse {
    #[serde(rename = "accessJwt")]
    access_jwt: String,
    #[serde(rename = "refreshJwt")]
    refresh_jwt: String,
}

#[derive(Deserialize)]
struct NotificationListResponse {
    notifications: Vec<Notification>,
    cursor: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct Notification {
    uri: String,
    cid: String,
    author: NotificationAuthor,
    reason: String,
    record: Option<serde_json::Value>,
    #[serde(rename = "isRead")]
    is_read: bool,
    #[serde(rename = "indexedAt")]
    indexed_at: String,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct NotificationAuthor {
    did: String,
    handle: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

/// AT Protocol record for creating a post.
#[derive(Serialize)]
struct CreateRecordRequest {
    repo: String,
    collection: String,
    record: PostRecord,
}

#[derive(Serialize)]
struct PostRecord {
    #[serde(rename = "$type")]
    record_type: String,
    text: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply: Option<ReplyRef>,
}

#[derive(Serialize)]
struct ReplyRef {
    root: PostRef,
    parent: PostRef,
}

#[derive(Serialize)]
struct PostRef {
    uri: String,
    cid: String,
}

impl BlueskyChannel {
    pub fn new(handle: String, app_password: String) -> Self {
        Self {
            handle,
            app_password,
            auth: Mutex::new(BlueskyAuth {
                access_jwt: String::new(),
                refresh_jwt: String::new(),
                did: String::new(),
                expires_at: Instant::now(),
            }),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.bluesky")
    }

    /// Create a new session with handle + app password.
    async fn create_session(&self) -> Result<()> {
        let client = self.http_client();
        let resp = client
            .post(format!("{BSKY_API_BASE}/com.atproto.server.createSession"))
            .json(&serde_json::json!({
                "identifier": self.handle,
                "password": self.app_password,
            }))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("Bluesky createSession failed ({status}): {body}");
        }

        let session: CreateSessionResponse = resp.json().await?;
        let mut auth = self.auth.lock();
        auth.access_jwt = session.access_jwt;
        auth.refresh_jwt = session.refresh_jwt;
        auth.did = session.did;
        // AT Protocol JWTs typically last ~2 hours; refresh well before that.
        auth.expires_at = Instant::now() + Duration::from_secs(90 * 60);
        Ok(())
    }

    /// Refresh an existing session.
    async fn refresh_session(&self) -> Result<()> {
        let refresh_jwt = {
            let auth = self.auth.lock();
            auth.refresh_jwt.clone()
        };

        if refresh_jwt.is_empty() {
            return self.create_session().await;
        }

        let client = self.http_client();
        let resp = client
            .post(format!("{BSKY_API_BASE}/com.atproto.server.refreshSession"))
            .bearer_auth(&refresh_jwt)
            .send()
            .await?;

        if !resp.status().is_success() {
            // Refresh failed — fall back to full re-auth
            tracing::warn!("Bluesky session refresh failed, re-authenticating");
            return self.create_session().await;
        }

        let refreshed: RefreshSessionResponse = resp.json().await?;
        let mut auth = self.auth.lock();
        auth.access_jwt = refreshed.access_jwt;
        auth.refresh_jwt = refreshed.refresh_jwt;
        auth.expires_at = Instant::now() + Duration::from_secs(90 * 60);
        Ok(())
    }

    /// Get a valid access JWT, refreshing if expired.
    async fn get_access_jwt(&self) -> Result<String> {
        {
            let auth = self.auth.lock();
            if !auth.access_jwt.is_empty() && Instant::now() < auth.expires_at {
                return Ok(auth.access_jwt.clone());
            }
        }
        self.refresh_session().await?;
        let auth = self.auth.lock();
        Ok(auth.access_jwt.clone())
    }

    /// Get the DID for the authenticated account.
    fn get_did(&self) -> String {
        self.auth.lock().did.clone()
    }

    /// Parse a notification into a ChannelMessage (only processes mentions).
    fn parse_notification(&self, notif: &Notification) -> Option<ChannelMessage> {
        // Only process mentions
        if notif.reason != "mention" && notif.reason != "reply" {
            return None;
        }

        // Skip already-read notifications
        if notif.is_read {
            return None;
        }

        // Skip own posts
        if notif.author.did == self.get_did() {
            return None;
        }

        // Extract text from the record
        let text = notif
            .record
            .as_ref()
            .and_then(|r| r.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");

        if text.is_empty() {
            return None;
        }

        // Parse timestamp from indexedAt (ISO 8601)
        let timestamp = chrono::DateTime::parse_from_rfc3339(&notif.indexed_at)
            .map(|dt| dt.timestamp().cast_unsigned())
            .unwrap_or(0);

        // Extract CID from the record for reply references
        let cid = notif
            .record
            .as_ref()
            .and_then(|r| r.get("cid"))
            .and_then(|c| c.as_str())
            .unwrap_or(&notif.cid);

        // The reply target encodes the URI and CID needed for threading
        let reply_target = format!("{}|{}", notif.uri, cid);

        Some(ChannelMessage {
            id: format!("bluesky_{}", notif.cid),
            sender: notif.author.handle.clone(),
            reply_target,
            content: text.to_string(),
            channel: "bluesky".to_string(),
            timestamp,
            thread_ts: Some(notif.uri.clone()),
        })
    }

    /// Mark notifications as read up to a given timestamp.
    async fn update_seen(&self, seen_at: &str) -> Result<()> {
        let token = self.get_access_jwt().await?;
        let client = self.http_client();

        let resp = client
            .post(format!("{BSKY_API_BASE}/app.bsky.notification.updateSeen"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "seenAt": seen_at }))
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::warn!("Bluesky updateSeen failed: {}", resp.status());
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for BlueskyChannel {
    fn name(&self) -> &str {
        "bluesky"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let token = self.get_access_jwt().await?;
        let did = self.get_did();
        let client = self.http_client();

        let now = chrono::Utc::now().to_rfc3339();

        // Parse reply reference from recipient if present (format: "uri|cid")
        let reply = if message.recipient.contains('|') {
            let parts: Vec<&str> = message.recipient.splitn(2, '|').collect();
            if parts.len() == 2 {
                let uri = parts[0];
                let cid = parts[1];
                Some(ReplyRef {
                    root: PostRef {
                        uri: uri.to_string(),
                        cid: cid.to_string(),
                    },
                    parent: PostRef {
                        uri: uri.to_string(),
                        cid: cid.to_string(),
                    },
                })
            } else {
                None
            }
        } else {
            None
        };

        // Bluesky posts have a 300-character limit (grapheme clusters).
        // For longer content, truncate with an indicator.
        let text = if message.content.len() > 300 {
            format!("{}...", &message.content[..297])
        } else {
            message.content.clone()
        };

        let request = CreateRecordRequest {
            repo: did,
            collection: "app.bsky.feed.post".to_string(),
            record: PostRecord {
                record_type: "app.bsky.feed.post".to_string(),
                text,
                created_at: now,
                reply,
            },
        };

        let resp = client
            .post(format!("{BSKY_API_BASE}/com.atproto.repo.createRecord"))
            .bearer_auth(&token)
            .json(&request)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("Bluesky post failed ({status}): {body}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Initial auth
        self.create_session().await?;

        tracing::info!("Bluesky channel listening as @{}...", self.handle);

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let token = match self.get_access_jwt().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("Bluesky auth error: {e}");
                    continue;
                }
            };

            let client = self.http_client();
            let resp = match client
                .get(format!(
                    "{BSKY_API_BASE}/app.bsky.notification.listNotifications"
                ))
                .bearer_auth(&token)
                .query(&[("limit", "25")])
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Bluesky poll error: {e}");
                    continue;
                }
            };

            if !resp.status().is_success() {
                tracing::warn!("Bluesky notifications failed: {}", resp.status());
                continue;
            }

            let listing: NotificationListResponse = match resp.json().await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("Bluesky parse error: {e}");
                    continue;
                }
            };

            let mut latest_indexed_at: Option<String> = None;
            for notif in &listing.notifications {
                if let Some(msg) = self.parse_notification(notif) {
                    latest_indexed_at = Some(notif.indexed_at.clone());
                    if tx.send(msg).await.is_err() {
                        return Ok(());
                    }
                }
            }

            // Mark as seen
            if let Some(ref seen_at) = latest_indexed_at {
                if let Err(e) = self.update_seen(seen_at).await {
                    tracing::warn!("Bluesky updateSeen error: {e}");
                }
            }

            let _ = &listing.cursor; // cursor available for pagination if needed
        }
    }

    async fn health_check(&self) -> bool {
        self.get_access_jwt().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> BlueskyChannel {
        let ch = BlueskyChannel::new("testbot.bsky.social".into(), "app-password".into());
        // Seed auth with a DID for tests
        {
            let mut auth = ch.auth.lock();
            auth.did = "did:plc:test123".into();
        }
        ch
    }

    fn make_notification(
        reason: &str,
        handle: &str,
        did: &str,
        text: &str,
        is_read: bool,
    ) -> Notification {
        Notification {
            uri: format!("at://{did}/app.bsky.feed.post/abc123"),
            cid: "bafyreitest123".into(),
            author: NotificationAuthor {
                did: did.into(),
                handle: handle.into(),
                display_name: None,
            },
            reason: reason.into(),
            record: Some(serde_json::json!({ "text": text })),
            is_read,
            indexed_at: "2026-01-15T10:00:00.000Z".into(),
        }
    }

    #[test]
    fn parse_mention_notification() {
        let ch = make_channel();
        let notif = make_notification(
            "mention",
            "user1.bsky.social",
            "did:plc:user1",
            "@testbot hello",
            false,
        );

        let msg = ch.parse_notification(&notif).unwrap();
        assert_eq!(msg.sender, "user1.bsky.social");
        assert_eq!(msg.content, "@testbot hello");
        assert_eq!(msg.channel, "bluesky");
        assert!(msg.id.starts_with("bluesky_"));
    }

    #[test]
    fn parse_reply_notification() {
        let ch = make_channel();
        let notif = make_notification(
            "reply",
            "user2.bsky.social",
            "did:plc:user2",
            "thanks for the info!",
            false,
        );

        let msg = ch.parse_notification(&notif).unwrap();
        assert_eq!(msg.sender, "user2.bsky.social");
        assert_eq!(msg.content, "thanks for the info!");
    }

    #[test]
    fn skip_read_notifications() {
        let ch = make_channel();
        let notif = make_notification(
            "mention",
            "user1.bsky.social",
            "did:plc:user1",
            "old message",
            true,
        );

        assert!(ch.parse_notification(&notif).is_none());
    }

    #[test]
    fn skip_own_notifications() {
        let ch = make_channel();
        let notif = make_notification(
            "mention",
            "testbot.bsky.social",
            "did:plc:test123", // same as seeded DID
            "self message",
            false,
        );

        assert!(ch.parse_notification(&notif).is_none());
    }

    #[test]
    fn skip_like_notifications() {
        let ch = make_channel();
        let notif = make_notification(
            "like",
            "user1.bsky.social",
            "did:plc:user1",
            "liked post",
            false,
        );

        assert!(ch.parse_notification(&notif).is_none());
    }

    #[test]
    fn skip_empty_text() {
        let ch = make_channel();
        let notif = make_notification("mention", "user1.bsky.social", "did:plc:user1", "", false);

        assert!(ch.parse_notification(&notif).is_none());
    }

    #[test]
    fn reply_target_encoding() {
        let ch = make_channel();
        let notif = make_notification(
            "mention",
            "user1.bsky.social",
            "did:plc:user1",
            "hello",
            false,
        );

        let msg = ch.parse_notification(&notif).unwrap();
        // reply_target should contain URI|CID
        assert!(msg.reply_target.contains('|'));
        let parts: Vec<&str> = msg.reply_target.splitn(2, '|').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("at://"));
    }

    #[test]
    fn send_message_formatting() {
        // Verify reply target parsing
        let reply_target = "at://did:plc:user1/app.bsky.feed.post/abc|bafyreitest";
        let parts: Vec<&str> = reply_target.splitn(2, '|').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "at://did:plc:user1/app.bsky.feed.post/abc");
        assert_eq!(parts[1], "bafyreitest");
    }
}
