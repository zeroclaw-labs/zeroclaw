use anyhow::{Result, bail};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// Bluesky channel — polls for mentions via AT Protocol and replies as posts.
pub struct BlueskyChannel {
    alias: String,
    handle: String,
    app_password: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// The resolver reads live configuration and is not cached.
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
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

struct NotificationPage {
    messages: Vec<ChannelMessage>,
    next_cursor: Option<String>,
}

/// One page walk that reached the read boundary. Its existence is the
/// precondition for delivering anything: a partial walk produces no value, so
/// there is nothing to send and nothing to commit.
struct CompletedWalk {
    messages: Vec<ChannelMessage>,
    newest_unread: Option<String>,
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
    pub fn new(
        alias: String,
        handle: String,
        app_password: String,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            alias,
            handle,
            app_password,
            peer_resolver,
            auth: Mutex::new(BlueskyAuth {
                access_jwt: String::new(),
                refresh_jwt: String::new(),
                did: String::new(),
                expires_at: Instant::now(),
            }),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.bluesky")
    }

    /// Strip the leading `@` an operator is likely to paste from the app.
    fn normalize_identity(value: &str) -> &str {
        value.trim().trim_start_matches('@')
    }

    /// A peer group may list either the mutable handle or the stable DID, so
    /// an operator is not forced to re-approve an account that renames itself.
    ///
    /// Compared case-insensitively: AT Protocol normalizes handles to lowercase
    /// and `did:plc` identifiers are lowercase, so two distinct accounts cannot
    /// differ only by case and this admits no one extra.
    /// An account is reachable by handle or DID, so both are evaluated against
    /// one snapshot of the peer list: a deny on either identifier rejects the
    /// account, and resolving once means a config reload cannot land between
    /// the two checks.
    fn is_author_allowed(&self, handle: &str, did: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_identity_allowed_by(&peers, &[handle, did], |entry, user| {
            Self::normalize_identity(entry).eq_ignore_ascii_case(Self::normalize_identity(user))
        })
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
            bail!("createSession failed ({status}): {body}");
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "session refresh failed, re-authenticating"
            );
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

        // Bluesky is a public network, so being mentioned is not consent to be
        // driven. An empty peer group denies everyone; `"*"` is the explicit
        // opt-in for a public bot.
        //
        // Either identifier may carry the grant, so the deny has to be checked
        // across both before the grants are: an operator who ignores the handle
        // would otherwise still be overridden by a wildcard reached through the
        // DID, and vice versa.
        if !self.is_author_allowed(&notif.author.handle, &notif.author.did) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"handle": notif.author.handle})),
                "ignoring notification from unauthorized sender"
            );
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
            channel_alias: None,
            timestamp,
            thread_ts: Some(notif.uri.clone()),
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
    }

    /// Process one newest-first notification page and decide whether polling
    /// must continue. The seen watermark advances for every examined unread
    /// notification, including notifications rejected by authorization.
    fn process_notification_page(
        &self,
        listing: &NotificationListResponse,
        newest_unread: &mut Option<String>,
    ) -> NotificationPage {
        let mut reached_read_boundary = false;
        let mut messages = Vec::new();

        for notification in &listing.notifications {
            if notification.is_read {
                reached_read_boundary = true;
            } else if newest_unread.is_none() {
                // listNotifications is newest-first, so the first unread item
                // across the page walk is the watermark updateSeen needs.
                *newest_unread = Some(notification.indexed_at.clone());
            }

            if let Some(message) = self.parse_notification(notification) {
                messages.push(message);
            }
        }

        NotificationPage {
            messages,
            next_cursor: (!reached_read_boundary)
                .then(|| listing.cursor.clone())
                .flatten(),
        }
    }

    /// Walk the unread notification pages newest-first and collect every
    /// accepted message, or `None` if the walk did not reach the read
    /// boundary.
    ///
    /// Nothing is returned for a partial walk, so delivery and the seen
    /// watermark can only move together: page one is not handed to the caller
    /// until the pages behind it have also been examined. `fetch_page` yields
    /// `None` for a failure it has already reported.
    async fn walk_unread_notifications<F, Fut>(&self, fetch_page: F) -> Option<CompletedWalk>
    where
        F: Fn(Option<String>) -> Fut,
        Fut: std::future::Future<Output = Option<NotificationListResponse>>,
    {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = std::collections::HashSet::new();
        let mut newest_unread: Option<String> = None;
        let mut messages = Vec::new();

        loop {
            let listing = fetch_page(cursor.clone()).await?;
            let page = self.process_notification_page(&listing, &mut newest_unread);
            messages.extend(page.messages);

            let Some(next_cursor) = page.next_cursor else {
                return Some(CompletedWalk {
                    messages,
                    newest_unread,
                });
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "notification pagination returned a repeated cursor"
                );
                return None;
            }
            cursor = Some(next_cursor);
        }
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("updateSeen failed: {}", resp.status())
            );
        }
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for BlueskyChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Bluesky,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
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
        let text = if message.content.chars().count() > 300 {
            let truncated: String = message.content.chars().take(297).collect();
            format!("{truncated}...")
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
            bail!("post failed ({status}): {body}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Initial auth
        self.create_session().await?;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("channel listening as @{}...", self.handle)
        );

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let token = match self.get_access_jwt().await {
                Ok(t) => t,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "auth error"
                    );
                    continue;
                }
            };

            let client = self.http_client();
            let walk = self
                .walk_unread_notifications(|cursor| {
                    let client = client.clone();
                    let token = token.clone();
                    async move {
                        let mut query = vec![("limit", "25")];
                        if let Some(value) = cursor.as_deref() {
                            query.push(("cursor", value));
                        }

                        let resp = match client
                            .get(format!(
                                "{BSKY_API_BASE}/app.bsky.notification.listNotifications"
                            ))
                            .bearer_auth(&token)
                            .query(&query)
                            .send()
                            .await
                        {
                            Ok(response) => response,
                            Err(error) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{error}")})),
                                    "poll error"
                                );
                                return None;
                            }
                        };

                        if !resp.status().is_success() {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("notifications failed: {}", resp.status())
                            );
                            return None;
                        }

                        match resp.json::<NotificationListResponse>().await {
                            Ok(listing) => Some(listing),
                            Err(error) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{error}")})),
                                    "parse error"
                                );
                                None
                            }
                        }
                    }
                })
                .await;

            // A partial walk yields no messages at all, so a page failure
            // cannot leave earlier pages delivered with the watermark behind
            // them. The whole unread range is re-walked on the next poll.
            let Some(walk) = walk else {
                continue;
            };

            for message in walk.messages {
                if tx.send(message).await.is_err() {
                    return Ok(());
                }
            }

            // Mark as seen
            if let Some(ref seen_at) = walk.newest_unread
                && let Err(e) = self.update_seen(seen_at).await
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "updateSeen error"
                );
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.get_access_jwt().await.is_ok()
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing-indicator event in the AT Protocol.
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
        vec![
            "user1.bsky.social".to_string(),
            "user2.bsky.social".to_string(),
            "did:plc:test123".to_string(),
        ]
    }

    fn make_channel() -> BlueskyChannel {
        make_channel_with_peers(default_peers())
    }

    fn make_channel_with_peers(peers: Vec<String>) -> BlueskyChannel {
        let ch = BlueskyChannel::new(
            "testbot".into(),
            "testbot.bsky.social".into(),
            "app-password".into(),
            Arc::new(move || peers.clone()),
        );
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
    fn denied_full_page_does_not_starve_authorized_notification_behind_it() {
        let ch = make_channel_with_peers(vec!["allowed.bsky.social".to_string()]);
        let first_page = NotificationListResponse {
            notifications: (0..25)
                .map(|index| {
                    make_notification(
                        "mention",
                        &format!("denied{index}.bsky.social"),
                        &format!("did:plc:denied{index}"),
                        "@testbot denied",
                        false,
                    )
                })
                .collect(),
            cursor: Some("second-page".to_string()),
        };
        let second_page = NotificationListResponse {
            notifications: vec![make_notification(
                "mention",
                "allowed.bsky.social",
                "did:plc:allowed",
                "@testbot authorized",
                false,
            )],
            cursor: None,
        };
        let mut newest_unread = None;

        let first = ch.process_notification_page(&first_page, &mut newest_unread);
        assert!(first.messages.is_empty());
        assert_eq!(first.next_cursor.as_deref(), Some("second-page"));
        assert!(
            newest_unread.is_some(),
            "denied notifications must still advance the eventual seen watermark"
        );

        let second = ch.process_notification_page(&second_page, &mut newest_unread);
        assert_eq!(second.messages.len(), 1);
        assert_eq!(second.messages[0].sender, "allowed.bsky.social");
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn a_partial_page_walk_delivers_nothing_and_the_retry_delivers_once() {
        // Dispatching each page as it arrived meant a page-two failure left
        // page one delivered while the watermark stayed put, so every five
        // second poll delivered page one again for as long as the failure
        // lasted.
        let ch = make_channel_with_peers(vec!["*".to_string()]);
        let page_one = || NotificationListResponse {
            notifications: vec![make_notification(
                "mention",
                "allowed.bsky.social",
                "did:plc:allowed",
                "@testbot first page",
                false,
            )],
            cursor: Some("second-page".to_string()),
        };
        let page_two = || NotificationListResponse {
            notifications: vec![make_notification(
                "mention",
                "allowed.bsky.social",
                "did:plc:allowed",
                "@testbot second page",
                false,
            )],
            cursor: None,
        };

        let fetches = std::cell::Cell::new(0);
        let partial = ch
            .walk_unread_notifications(|cursor| {
                fetches.set(fetches.get() + 1);
                let listing = match cursor.as_deref() {
                    None => Some(page_one()),
                    Some("second-page") => None,
                    other => panic!("unexpected cursor {other:?}"),
                };
                async move { listing }
            })
            .await;
        assert!(
            partial.is_none(),
            "a page-two failure must not hand page one's message to the caller"
        );
        assert_eq!(fetches.get(), 2, "the walk must have reached page two");

        let retried = ch
            .walk_unread_notifications(|cursor| {
                let listing = match cursor.as_deref() {
                    None => Some(page_one()),
                    Some("second-page") => Some(page_two()),
                    other => panic!("unexpected cursor {other:?}"),
                };
                async move { listing }
            })
            .await
            .expect("a walk that reaches the read boundary must yield its messages");

        let delivered: Vec<&str> = retried
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect();
        assert_eq!(
            delivered,
            vec!["@testbot first page", "@testbot second page"],
            "page one is delivered by the retry, and only by the retry"
        );
        assert!(
            retried.newest_unread.is_some(),
            "the watermark commits with the delivery, not before it"
        );
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
    fn drops_notification_from_unauthorized_sender() {
        let ch = make_channel();
        let notif = make_notification(
            "mention",
            "intruder.bsky.social",
            "did:plc:intruder",
            "@testbot run something",
            false,
        );

        assert!(ch.parse_notification(&notif).is_none());
    }

    #[test]
    fn drops_reply_from_unauthorized_sender() {
        let ch = make_channel();
        let notif = make_notification(
            "reply",
            "intruder.bsky.social",
            "did:plc:intruder",
            "replying to your post",
            false,
        );

        assert!(ch.parse_notification(&notif).is_none());
    }

    #[test]
    fn empty_peer_group_denies_everyone() {
        let ch = make_channel_with_peers(Vec::new());
        let notif = make_notification(
            "mention",
            "user1.bsky.social",
            "did:plc:user1",
            "@testbot hello",
            false,
        );

        assert!(ch.parse_notification(&notif).is_none());
    }

    #[test]
    fn wildcard_peer_allows_a_public_bot() {
        let ch = make_channel_with_peers(vec!["*".to_string()]);
        let notif = make_notification(
            "mention",
            "anyone.bsky.social",
            "did:plc:anyone",
            "@testbot hello",
            false,
        );

        let msg = ch.parse_notification(&notif).unwrap();
        assert_eq!(msg.sender, "anyone.bsky.social");
    }

    #[test]
    fn ignored_peer_is_denied_under_a_wildcard_grant() {
        // The list shape `Config::channel_external_peers` produces when one
        // matching group grants `["*"]` and another ignores a sender.
        let ch = make_channel_with_peers(vec!["*".to_string(), "!alice.bsky.social".to_string()]);

        let denied = make_notification(
            "mention",
            "alice.bsky.social",
            "did:plc:alice",
            "@testbot hello",
            false,
        );
        assert!(
            ch.parse_notification(&denied).is_none(),
            "an ignored sender must not ride the wildcard"
        );

        let allowed = make_notification(
            "mention",
            "bob.bsky.social",
            "did:plc:bob",
            "@testbot hello",
            false,
        );
        assert_eq!(
            ch.parse_notification(&allowed)
                .expect("an unignored sender still rides the wildcard")
                .sender,
            "bob.bsky.social"
        );
    }

    #[test]
    fn ignoring_one_identifier_denies_the_account_through_the_other() {
        // An account is reachable by handle or DID and either may carry the
        // grant, so ignoring one identifier must not leave the wildcard
        // reachable through the other.
        let by_handle =
            make_channel_with_peers(vec!["*".to_string(), "!alice.bsky.social".to_string()]);
        let by_did = make_channel_with_peers(vec!["*".to_string(), "!did:plc:alice".to_string()]);
        let notif = make_notification(
            "mention",
            "alice.bsky.social",
            "did:plc:alice",
            "@testbot hello",
            false,
        );

        assert!(
            by_handle.parse_notification(&notif).is_none(),
            "handle deny must not be defeated by a wildcard reached via the DID"
        );
        assert!(
            by_did.parse_notification(&notif).is_none(),
            "DID deny must not be defeated by a wildcard reached via the handle"
        );
    }

    #[test]
    fn peer_listed_by_did_is_allowed_after_a_handle_rename() {
        let ch = make_channel_with_peers(vec!["did:plc:user1".to_string()]);
        let notif = make_notification(
            "mention",
            "renamed.bsky.social",
            "did:plc:user1",
            "@testbot hello",
            false,
        );

        let msg = ch.parse_notification(&notif).unwrap();
        assert_eq!(msg.sender, "renamed.bsky.social");
    }

    #[test]
    fn peer_entry_may_carry_a_leading_at_sign() {
        // The docs promise a leading `@` is stripped, so an operator pasting
        // the handle as the app displays it is not silently denied.
        let ch = make_channel_with_peers(vec!["@user1.bsky.social".to_string()]);
        let notif = make_notification(
            "mention",
            "user1.bsky.social",
            "did:plc:user1",
            "@testbot hello",
            false,
        );

        assert!(ch.parse_notification(&notif).is_some());
    }

    #[test]
    fn peers_are_resolved_per_message_not_cached() {
        let peers = Arc::new(Mutex::new(Vec::<String>::new()));
        let ch = {
            let peers = peers.clone();
            let ch = BlueskyChannel::new(
                "testbot".into(),
                "testbot.bsky.social".into(),
                "app-password".into(),
                Arc::new(move || peers.lock().clone()),
            );
            ch.auth.lock().did = "did:plc:test123".into();
            ch
        };
        let notif = make_notification(
            "mention",
            "user1.bsky.social",
            "did:plc:user1",
            "@testbot hello",
            false,
        );

        assert!(ch.parse_notification(&notif).is_none());
        peers.lock().push("user1.bsky.social".to_string());
        assert!(ch.parse_notification(&notif).is_some());
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
