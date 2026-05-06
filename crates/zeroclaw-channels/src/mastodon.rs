//! Mastodon channel — talks ActivityPub via the standard Mastodon REST + Streaming APIs.
//!
//! Works with any Mastodon-compatible instance (mastodon.social, fosstodon.org,
//! hachyderm.io, Pleroma, GoToSocial). The channel reads inbound mentions and
//! direct messages from the user-stream WebSocket and posts replies back as
//! statuses.
//!
//! # Auth
//! Personal access token minted via instance Settings → Development → New
//! Application. Required scopes: `read:notifications`, `write:statuses`,
//! `read:accounts`. The OAuth code-grant flow is intentionally not implemented;
//! the manual mint is fine for a single bot account.
//!
//! # Inbound
//! Subscribes to `wss://{instance}/api/v1/streaming?stream=user&access_token=…`
//! and routes `notification` events whose `type` is `mention` or `direct` into
//! `ChannelMessage`s. Reconnects with exponential backoff on disconnect. After
//! three consecutive connect failures the channel falls back to polling
//! `/api/v1/notifications` every `poll_interval_secs`.
//!
//! # Outbound
//! `POST {instance}/api/v1/statuses` with `status`, `visibility`, and an
//! optional `in_reply_to_id`. The status text is split into ≤500-character
//! chunks at sentence/word boundaries; each chunk re-applies the recipient's
//! `@mention` so direct/private visibility continues to deliver every part.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::MastodonVisibility;

const MASTODON_STATUS_LIMIT: usize = 500;
const STREAM_RETRY_THRESHOLD: u32 = 3;
const STREAM_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const STREAM_BACKOFF_MAX: Duration = Duration::from_secs(60);
const POLL_NOTIFICATION_LIMIT: u32 = 40;

/// Mastodon channel — see module docs.
pub struct MastodonChannel {
    instance_url: String,
    access_token: String,
    allowed_users: Vec<String>,
    mention_only: bool,
    visibility: MastodonVisibility,
    poll_interval: Duration,
    /// Bot account info, populated by `verify_credentials` on first listen.
    identity: Mutex<Option<BotIdentity>>,
}

#[derive(Clone)]
struct BotIdentity {
    /// Numeric account id (used to filter self-notifications).
    id: String,
    /// `username` (no `@instance` suffix) — used for self-mention detection.
    username: String,
    /// `acct` form: bare username for local, `user@instance` for remote.
    acct: String,
}

#[derive(Deserialize)]
struct VerifyCredentialsResponse {
    id: String,
    username: String,
    acct: String,
}

/// One JSON event from the Streaming API. Field `payload` is itself a
/// JSON-encoded string that we re-decode on demand.
#[derive(Deserialize)]
struct StreamEnvelope {
    event: String,
    payload: String,
}

#[derive(Deserialize)]
struct Notification {
    #[allow(dead_code)]
    id: String,
    #[serde(rename = "type")]
    notification_type: String,
    account: NotificationAccount,
    status: Option<Status>,
}

#[derive(Deserialize)]
struct NotificationAccount {
    id: String,
    #[allow(dead_code)]
    username: String,
    acct: String,
}

#[derive(Deserialize)]
struct Status {
    id: String,
    content: String,
    visibility: String,
    #[serde(default)]
    mentions: Vec<StatusMention>,
    #[serde(default)]
    in_reply_to_id: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
struct StatusMention {
    #[allow(dead_code)]
    id: String,
    acct: String,
    #[allow(dead_code)]
    username: String,
}

#[derive(Serialize)]
struct PostStatusRequest<'a> {
    status: &'a str,
    visibility: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    in_reply_to_id: Option<&'a str>,
}

#[derive(Deserialize)]
struct PostStatusResponse {
    id: String,
}

impl MastodonChannel {
    pub fn new(
        instance_url: String,
        access_token: String,
        allowed_users: Vec<String>,
        mention_only: bool,
        visibility: MastodonVisibility,
        poll_interval_secs: u64,
    ) -> Self {
        Self {
            instance_url: normalize_instance_url(&instance_url),
            access_token,
            allowed_users,
            mention_only,
            visibility,
            poll_interval: Duration::from_secs(poll_interval_secs.max(5)),
            identity: Mutex::new(None),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.mastodon")
    }

    fn rest_url(&self, path: &str) -> String {
        format!("{}{}", self.instance_url, path)
    }

    fn streaming_ws_url(&self) -> String {
        let ws_base = self.instance_url.replacen("https://", "wss://", 1);
        let ws_base = ws_base.replacen("http://", "ws://", 1);
        format!(
            "{ws_base}/api/v1/streaming?stream=user&access_token={}",
            urlencoding::encode(&self.access_token)
        )
    }

    fn identity(&self) -> Option<BotIdentity> {
        self.identity.lock().clone()
    }

    /// Resolve `verify_credentials` once per listen loop; cache locally.
    async fn ensure_identity(&self) -> Result<BotIdentity> {
        if let Some(id) = self.identity() {
            return Ok(id);
        }
        let resp = self
            .http_client()
            .get(self.rest_url("/api/v1/accounts/verify_credentials"))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .context("Mastodon verify_credentials request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Mastodon verify_credentials returned {status}: {body}");
        }
        let creds: VerifyCredentialsResponse = resp
            .json()
            .await
            .context("Mastodon verify_credentials returned non-JSON body")?;
        let identity = BotIdentity {
            id: creds.id,
            username: creds.username,
            acct: creds.acct,
        };
        *self.identity.lock() = Some(identity.clone());
        Ok(identity)
    }

    /// Convert a raw `Notification` into a `ChannelMessage`, applying the
    /// mention-only and allowlist filters. Returns `None` when the
    /// notification should be dropped.
    fn parse_notification(
        &self,
        notif: &Notification,
        identity: &BotIdentity,
    ) -> Option<ChannelMessage> {
        // Only mentions and direct messages are considered prompts. Other
        // notification types (favourite, reblog, follow, poll) are ignored.
        if notif.notification_type != "mention" {
            return None;
        }

        // Don't react to our own messages — Mastodon will deliver
        // self-mentions in a self-thread.
        if notif.account.id == identity.id {
            return None;
        }

        if !is_account_allowed(&notif.account.acct, &self.allowed_users) {
            tracing::debug!(
                "Mastodon: dropping mention from {} (not in allowed_users)",
                notif.account.acct
            );
            return None;
        }

        let status = notif.status.as_ref()?;

        // mention_only is the default — drop anything that doesn't actually
        // @-mention us. Direct visibility always counts as a mention because
        // Mastodon requires the @ tag for delivery.
        if self.mention_only
            && status.visibility != "direct"
            && !status_mentions_account(status, identity)
        {
            tracing::debug!(
                "Mastodon: dropping non-mention status (mention_only=true, visibility={})",
                status.visibility
            );
            return None;
        }

        let plain = strip_html(&status.content).trim().to_string();
        if plain.is_empty() {
            return None;
        }

        let timestamp = chrono::DateTime::parse_from_rfc3339(&status.created_at)
            .map(|dt| dt.timestamp().cast_unsigned())
            .unwrap_or(0);

        Some(ChannelMessage {
            id: format!("mastodon_{}", status.id),
            sender: notif.account.acct.clone(),
            // Encode "{acct}|{status_id}" so reply-target preserves both the
            // recipient @mention and the parent status for threading.
            reply_target: format!("{}|{}", notif.account.acct, status.id),
            content: plain,
            channel: "mastodon".to_string(),
            timestamp,
            thread_ts: status
                .in_reply_to_id
                .clone()
                .or_else(|| Some(status.id.clone())),
            interruption_scope_id: None,
            attachments: vec![],
        })
    }

    /// Listen via the Streaming API WebSocket. Returns `Err` on connect
    /// failure so the caller can decide whether to retry or fall back.
    async fn run_stream(
        &self,
        identity: &BotIdentity,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        let ws_url = self.streaming_ws_url();
        tracing::info!("Mastodon: connecting streaming API as @{}", identity.acct);
        let (ws_stream, _) =
            zeroclaw_config::schema::ws_connect_with_proxy(&ws_url, "channel.mastodon", None)
                .await
                .context("Mastodon streaming connect failed")?;
        let (mut write, mut read) = ws_stream.split();

        while let Some(frame) = read.next().await {
            let frame = frame.context("Mastodon stream read error")?;
            match frame {
                WsMessage::Text(text) => {
                    if let Some(msg) = self.handle_stream_text(text.as_str(), identity)
                        && tx.send(msg).await.is_err()
                    {
                        return Ok(());
                    }
                }
                WsMessage::Ping(payload) => {
                    let _ = write.send(WsMessage::Pong(payload)).await;
                }
                WsMessage::Close(_) => {
                    bail!("Mastodon stream closed by server");
                }
                _ => {}
            }
        }
        bail!("Mastodon stream ended unexpectedly");
    }

    fn handle_stream_text(&self, text: &str, identity: &BotIdentity) -> Option<ChannelMessage> {
        let envelope: StreamEnvelope = match serde_json::from_str(text) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("Mastodon: skipping non-envelope frame: {e}");
                return None;
            }
        };
        if envelope.event != "notification" {
            return None;
        }
        let notif: Notification = match serde_json::from_str(&envelope.payload) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("Mastodon: notification payload parse error: {e}");
                return None;
            }
        };
        self.parse_notification(&notif, identity)
    }

    /// Polling fallback used when the WebSocket cannot stay connected. Reads
    /// `/api/v1/notifications?types[]=mention` and dedupes by max id.
    async fn run_polling(
        &self,
        identity: &BotIdentity,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        let mut since_id: Option<String> = None;
        let client = self.http_client();
        loop {
            tokio::time::sleep(self.poll_interval).await;
            let mut req = client
                .get(self.rest_url("/api/v1/notifications"))
                .bearer_auth(&self.access_token)
                .query(&[
                    ("types[]", "mention"),
                    ("limit", &POLL_NOTIFICATION_LIMIT.to_string()),
                ]);
            if let Some(ref since) = since_id {
                req = req.query(&[("since_id", since.as_str())]);
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Mastodon poll error: {e}");
                    continue;
                }
            };
            if !resp.status().is_success() {
                tracing::warn!("Mastodon poll status: {}", resp.status());
                continue;
            }
            let notifs: Vec<Notification> = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Mastodon poll parse error: {e}");
                    continue;
                }
            };
            // Mastodon returns notifications newest-first; iterate in reverse
            // so message ordering matches send order.
            for notif in notifs.iter().rev() {
                if let Some(msg) = self.parse_notification(notif, identity)
                    && tx.send(msg).await.is_err()
                {
                    return Ok(());
                }
            }
            if let Some(latest) = notifs.first() {
                since_id = Some(latest.id.clone());
            }
        }
    }

    /// Send the body as one or more statuses, each ≤500 chars, all threaded
    /// off `in_reply_to_id` if provided.
    async fn post_status_chunks(
        &self,
        chunks: Vec<String>,
        in_reply_to: Option<String>,
    ) -> Result<()> {
        let visibility = visibility_str(self.visibility);
        let client = self.http_client();
        let mut reply_to = in_reply_to;
        for chunk in chunks {
            let body = PostStatusRequest {
                status: &chunk,
                visibility,
                in_reply_to_id: reply_to.as_deref(),
            };
            let resp = client
                .post(self.rest_url("/api/v1/statuses"))
                .bearer_auth(&self.access_token)
                .json(&body)
                .send()
                .await
                .context("Mastodon POST /api/v1/statuses failed")?;
            let status = resp.status();
            if !status.is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                bail!("Mastodon status post returned {status}: {body_text}");
            }
            let posted: PostStatusResponse = resp
                .json()
                .await
                .context("Mastodon status post returned non-JSON body")?;
            // Thread continuation chunks behind the first chunk so the whole
            // chain reads as a connected reply.
            reply_to = Some(posted.id);
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for MastodonChannel {
    fn name(&self) -> &str {
        "mastodon"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let (acct_target, in_reply_to) =
            parse_recipient(&message.recipient, message.thread_ts.as_deref());
        let chunks = chunk_status(
            &message.content,
            acct_target.as_deref(),
            MASTODON_STATUS_LIMIT,
        );
        if chunks.is_empty() {
            return Ok(());
        }
        self.post_status_chunks(chunks, in_reply_to).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let identity = self.ensure_identity().await?;
        tracing::info!(
            "Mastodon channel listening as @{} (mention_only={}, visibility={:?})",
            identity.acct,
            self.mention_only,
            self.visibility
        );

        let mut consecutive_failures: u32 = 0;
        let mut backoff = STREAM_BACKOFF_INITIAL;
        loop {
            match self.run_stream(&identity, &tx).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    consecutive_failures += 1;
                    tracing::warn!("Mastodon stream error (attempt {consecutive_failures}): {e}");
                }
            }

            if consecutive_failures >= STREAM_RETRY_THRESHOLD {
                tracing::warn!(
                    "Mastodon: streaming unavailable after {STREAM_RETRY_THRESHOLD} attempts, falling back to polling every {}s",
                    self.poll_interval.as_secs()
                );
                return self.run_polling(&identity, &tx).await;
            }

            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(STREAM_BACKOFF_MAX);
        }
    }

    async fn health_check(&self) -> bool {
        self.ensure_identity().await.is_ok()
    }
}

fn normalize_instance_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

fn visibility_str(v: MastodonVisibility) -> &'static str {
    match v {
        MastodonVisibility::Direct => "direct",
        MastodonVisibility::Private => "private",
        MastodonVisibility::Unlisted => "unlisted",
        MastodonVisibility::Public => "public",
    }
}

/// Decode Mastodon-flavoured HTML status content into plain text. Mastodon
/// emits `<p>`-wrapped fragments separated by `<br />`; full-blown HTML
/// rendering isn't necessary for our purposes.
fn strip_html(html: &str) -> String {
    nanohtml2text::html2text(html)
}

/// Whether the status `mentions` array contains the bot account.
fn status_mentions_account(status: &Status, identity: &BotIdentity) -> bool {
    status.mentions.iter().any(|m| {
        m.acct.eq_ignore_ascii_case(&identity.acct)
            || m.acct.eq_ignore_ascii_case(&identity.username)
    })
}

/// Allowlist matching for Mastodon `acct` strings. `*` matches any account.
/// Entries are compared case-insensitively. Local-instance accounts may be
/// listed without the `@instance` suffix.
fn is_account_allowed(acct: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return false;
    }
    if allowlist.iter().any(|a| a == "*") {
        return true;
    }
    let normalized = acct.trim_start_matches('@').to_ascii_lowercase();
    let bare_local = normalized.split('@').next().unwrap_or("").to_string();
    allowlist.iter().any(|entry| {
        let e = entry.trim_start_matches('@').to_ascii_lowercase();
        e == normalized || e == bare_local
    })
}

/// Parse a `SendMessage.recipient` into `(target_acct, in_reply_to_id)`.
///
/// Recognised forms:
/// * `"alice@instance"` — new DM/post, no parent status.
/// * `"alice@instance|12345"` — reply to status `12345`.
/// * `""` — opportunistic broadcast post (no @-mention applied).
///
/// `thread_ts` overrides any in_reply_to embedded in the recipient when set.
fn parse_recipient(recipient: &str, thread_ts: Option<&str>) -> (Option<String>, Option<String>) {
    let (acct, embedded_reply) = match recipient.split_once('|') {
        Some((a, r)) => (a, Some(r)),
        None => (recipient, None),
    };
    let acct = acct.trim();
    let acct_opt = if acct.is_empty() {
        None
    } else {
        Some(acct.trim_start_matches('@').to_string())
    };
    let in_reply_to = thread_ts
        .map(|t| t.to_string())
        .or_else(|| embedded_reply.map(|s| s.to_string()))
        .filter(|s| !s.is_empty());
    (acct_opt, in_reply_to)
}

/// Split a body into chunks ≤ `limit` characters, prepending `@acct ` to each
/// chunk so non-public visibility levels keep delivering. Splits on sentence
/// boundary (`. `, `! `, `? `, `\n`) when possible, then word boundary, then
/// hard cut. Adds a `(i/N)` continuation marker when more than one chunk is
/// produced.
fn chunk_status(body: &str, acct_target: Option<&str>, limit: usize) -> Vec<String> {
    let body = body.trim();
    if body.is_empty() {
        return vec![];
    }
    let mention_prefix = acct_target.map(|a| format!("@{a} ")).unwrap_or_default();
    // Reserve space for the @mention and a possible "(99/99) " marker.
    const MARKER_RESERVE: usize = 8;
    let body_budget = limit
        .saturating_sub(mention_prefix.chars().count())
        .saturating_sub(MARKER_RESERVE)
        .max(1);

    let mut chunks: Vec<String> = Vec::new();
    let mut remaining: &str = body;
    while !remaining.is_empty() {
        if remaining.chars().count() <= body_budget {
            chunks.push(remaining.to_string());
            break;
        }
        let split_at = pick_split_point(remaining, body_budget);
        let (head, tail) = remaining.split_at(split_at);
        chunks.push(head.trim_end().to_string());
        remaining = tail.trim_start();
    }

    if chunks.len() == 1 {
        return vec![format!("{mention_prefix}{}", chunks[0])];
    }
    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| format!("{mention_prefix}({}/{total}) {c}", i + 1))
        .collect()
}

/// Choose a byte index ≤ char-budget that falls on a sensible boundary.
/// Prefers sentence enders, falls back to whitespace, then a hard char cut.
fn pick_split_point(text: &str, char_budget: usize) -> usize {
    // Walk forward by chars to find the byte index at the budget boundary.
    let mut budget_idx = text.len();
    for (i, (byte_idx, _)) in text.char_indices().enumerate() {
        if i == char_budget {
            budget_idx = byte_idx;
            break;
        }
    }
    let head = &text[..budget_idx];
    if let Some(idx) = head.rfind(['.', '!', '?', '\n']) {
        // Include the punctuation itself.
        return (idx + 1).min(budget_idx);
    }
    if let Some(idx) = head.rfind(char::is_whitespace) {
        return idx + 1;
    }
    budget_idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> BotIdentity {
        BotIdentity {
            id: "1".into(),
            username: "zeroclaw".into(),
            acct: "zeroclaw".into(),
        }
    }

    fn channel_with(allowlist: Vec<String>, mention_only: bool) -> MastodonChannel {
        let ch = MastodonChannel::new(
            "https://mastodon.social".into(),
            "test-token".into(),
            allowlist,
            mention_only,
            MastodonVisibility::Direct,
            60,
        );
        *ch.identity.lock() = Some(identity());
        ch
    }

    fn status(visibility: &str, content: &str, mention_acct: Option<&str>) -> Status {
        Status {
            id: "100".into(),
            content: content.into(),
            visibility: visibility.into(),
            mentions: mention_acct
                .map(|acct| {
                    vec![StatusMention {
                        id: "1".into(),
                        acct: acct.into(),
                        username: acct.split('@').next().unwrap_or(acct).into(),
                    }]
                })
                .unwrap_or_default(),
            in_reply_to_id: None,
            created_at: "2026-05-06T12:00:00.000Z".into(),
        }
    }

    fn notif_for(account_id: &str, account_acct: &str, status: Status) -> Notification {
        Notification {
            id: "200".into(),
            notification_type: "mention".into(),
            account: NotificationAccount {
                id: account_id.into(),
                username: account_acct
                    .split('@')
                    .next()
                    .unwrap_or(account_acct)
                    .into(),
                acct: account_acct.into(),
            },
            status: Some(status),
        }
    }

    #[test]
    fn normalize_instance_url_adds_https() {
        assert_eq!(
            normalize_instance_url("mastodon.social"),
            "https://mastodon.social"
        );
        assert_eq!(
            normalize_instance_url("https://mastodon.social/"),
            "https://mastodon.social"
        );
        assert_eq!(
            normalize_instance_url("  https://hachyderm.io/  "),
            "https://hachyderm.io"
        );
    }

    #[test]
    fn visibility_serializes_lowercase() {
        assert_eq!(visibility_str(MastodonVisibility::Direct), "direct");
        assert_eq!(visibility_str(MastodonVisibility::Private), "private");
        assert_eq!(visibility_str(MastodonVisibility::Unlisted), "unlisted");
        assert_eq!(visibility_str(MastodonVisibility::Public), "public");
    }

    #[test]
    fn allowlist_empty_denies_everyone() {
        assert!(!is_account_allowed("alice@mastodon.social", &[]));
    }

    #[test]
    fn allowlist_wildcard_allows_anyone() {
        let allow = vec!["*".into()];
        assert!(is_account_allowed("alice@mastodon.social", &allow));
        assert!(is_account_allowed("bob@hachyderm.io", &allow));
    }

    #[test]
    fn allowlist_matches_full_acct_case_insensitive() {
        let allow = vec!["Alice@Mastodon.Social".into()];
        assert!(is_account_allowed("alice@mastodon.social", &allow));
        assert!(!is_account_allowed("bob@mastodon.social", &allow));
    }

    #[test]
    fn allowlist_matches_local_bare_username() {
        // Operator listed `localfriend` (no @instance); the local-instance
        // notification arrives as bare `localfriend` from the verify_credentials
        // perspective.
        let allow = vec!["localfriend".into()];
        assert!(is_account_allowed("localfriend", &allow));
        assert!(is_account_allowed("localfriend@mastodon.social", &allow));
    }

    #[test]
    fn parse_recipient_simple_handle() {
        let (acct, reply) = parse_recipient("alice@mastodon.social", None);
        assert_eq!(acct.as_deref(), Some("alice@mastodon.social"));
        assert_eq!(reply, None);
    }

    #[test]
    fn parse_recipient_with_embedded_reply() {
        let (acct, reply) = parse_recipient("alice@mastodon.social|12345", None);
        assert_eq!(acct.as_deref(), Some("alice@mastodon.social"));
        assert_eq!(reply.as_deref(), Some("12345"));
    }

    #[test]
    fn parse_recipient_thread_ts_overrides_embedded() {
        let (acct, reply) = parse_recipient("alice@mastodon.social|12345", Some("99999"));
        assert_eq!(acct.as_deref(), Some("alice@mastodon.social"));
        assert_eq!(reply.as_deref(), Some("99999"));
    }

    #[test]
    fn parse_recipient_strips_leading_at() {
        let (acct, _) = parse_recipient("@alice@mastodon.social", None);
        assert_eq!(acct.as_deref(), Some("alice@mastodon.social"));
    }

    #[test]
    fn chunk_status_short_message_passes_through() {
        let chunks = chunk_status("Hello there", Some("alice@inst"), MASTODON_STATUS_LIMIT);
        assert_eq!(chunks, vec!["@alice@inst Hello there"]);
    }

    #[test]
    fn chunk_status_no_mention_when_acct_missing() {
        let chunks = chunk_status("Hello there", None, MASTODON_STATUS_LIMIT);
        assert_eq!(chunks, vec!["Hello there"]);
    }

    #[test]
    fn chunk_status_splits_long_body_at_sentence() {
        let body =
            "First sentence. Second sentence is here. Third one trails off because we keep typing.";
        let chunks = chunk_status(body, Some("a"), 60);
        assert!(
            chunks.len() >= 2,
            "expected at least two chunks, got {chunks:?}"
        );
        for (i, c) in chunks.iter().enumerate() {
            assert!(
                c.starts_with(&format!("@a ({}/", i + 1)),
                "missing mention/marker prefix on chunk {i}: {c:?}"
            );
            assert!(
                c.chars().count() <= 60,
                "chunk {i} exceeds limit: {c:?} ({} chars)",
                c.chars().count()
            );
        }
    }

    #[test]
    fn chunk_status_preserves_full_text_across_chunks() {
        let body = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu";
        let chunks = chunk_status(body, Some("a"), 50);
        assert!(chunks.len() >= 2);
        // Strip "@a (i/N) " prefix and concatenate; should equal the original
        // body up to whitespace normalization.
        let joined: Vec<String> = chunks
            .iter()
            .map(|c| {
                let stripped = c.trim_start_matches("@a ");
                let after_marker = stripped
                    .find(") ")
                    .map(|i| &stripped[i + 2..])
                    .unwrap_or(stripped);
                after_marker.to_string()
            })
            .collect();
        let recombined = joined.join(" ");
        assert_eq!(
            recombined.split_whitespace().collect::<Vec<_>>(),
            body.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn chunk_status_empty_body_returns_no_chunks() {
        let chunks = chunk_status("   ", Some("a"), MASTODON_STATUS_LIMIT);
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_notification_drops_self() {
        let ch = channel_with(vec!["*".into()], false);
        // identity.id = "1"; sender.id = "1" → drop
        let n = notif_for(
            "1",
            "zeroclaw",
            status("direct", "<p>hi me</p>", Some("zeroclaw")),
        );
        assert!(ch.parse_notification(&n, &identity()).is_none());
    }

    #[test]
    fn parse_notification_drops_outside_allowlist() {
        let ch = channel_with(vec!["alice@mastodon.social".into()], false);
        let n = notif_for(
            "42",
            "stranger@elsewhere.tld",
            status("direct", "<p>hello</p>", Some("zeroclaw")),
        );
        assert!(ch.parse_notification(&n, &identity()).is_none());
    }

    #[test]
    fn parse_notification_accepts_direct_with_mention() {
        let ch = channel_with(vec!["alice@mastodon.social".into()], true);
        let n = notif_for(
            "42",
            "alice@mastodon.social",
            status("direct", "<p>hi @zeroclaw</p>", Some("zeroclaw")),
        );
        let msg = ch
            .parse_notification(&n, &identity())
            .expect("expected message");
        assert_eq!(msg.sender, "alice@mastodon.social");
        assert_eq!(msg.channel, "mastodon");
        assert!(msg.content.contains("hi"));
        assert!(msg.id.starts_with("mastodon_"));
        assert_eq!(msg.reply_target, "alice@mastodon.social|100");
    }

    #[test]
    fn parse_notification_drops_public_without_mention_when_mention_only() {
        let ch = channel_with(vec!["*".into()], true);
        // public visibility, mention list empty
        let n = notif_for(
            "42",
            "alice@mastodon.social",
            status("public", "<p>hello world</p>", None),
        );
        assert!(ch.parse_notification(&n, &identity()).is_none());
    }

    #[test]
    fn parse_notification_accepts_public_when_mention_only_off() {
        let ch = channel_with(vec!["*".into()], false);
        let n = notif_for(
            "42",
            "alice@mastodon.social",
            status("public", "<p>hi everyone</p>", None),
        );
        assert!(ch.parse_notification(&n, &identity()).is_some());
    }

    #[test]
    fn parse_notification_drops_non_mention_type() {
        let ch = channel_with(vec!["*".into()], false);
        let mut n = notif_for(
            "42",
            "alice@mastodon.social",
            status("public", "<p>hi</p>", Some("zeroclaw")),
        );
        n.notification_type = "favourite".into();
        assert!(ch.parse_notification(&n, &identity()).is_none());
    }

    #[test]
    fn handle_stream_text_extracts_notification() {
        let ch = channel_with(vec!["*".into()], false);
        let payload = serde_json::json!({
            "id": "200",
            "type": "mention",
            "account": {
                "id": "42",
                "username": "alice",
                "acct": "alice@mastodon.social",
            },
            "status": {
                "id": "100",
                "content": "<p>hi @zeroclaw</p>",
                "visibility": "direct",
                "mentions": [{"id": "1", "acct": "zeroclaw", "username": "zeroclaw"}],
                "in_reply_to_id": null,
                "created_at": "2026-05-06T12:00:00.000Z",
            }
        })
        .to_string();
        let envelope = serde_json::json!({
            "stream": ["user"],
            "event": "notification",
            "payload": payload,
        })
        .to_string();
        let msg = ch
            .handle_stream_text(&envelope, &identity())
            .expect("expected message");
        assert_eq!(msg.sender, "alice@mastodon.social");
        assert!(msg.content.contains("hi"));
    }

    #[test]
    fn handle_stream_text_ignores_non_notification_events() {
        let ch = channel_with(vec!["*".into()], false);
        let envelope = serde_json::json!({
            "stream": ["user"],
            "event": "update",
            "payload": "{}",
        })
        .to_string();
        assert!(ch.handle_stream_text(&envelope, &identity()).is_none());
    }

    mod http_tests {
        use super::*;
        use serde_json::Value;
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn channel_for(server_uri: &str) -> MastodonChannel {
            MastodonChannel::new(
                server_uri.to_string(),
                "test-token".into(),
                vec!["*".into()],
                false,
                MastodonVisibility::Direct,
                60,
            )
        }

        #[tokio::test]
        async fn send_posts_status_with_visibility_and_mention() {
            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/api/v1/statuses"))
                .and(body_partial_json(serde_json::json!({
                    "status": "@alice@mastodon.social hello there",
                    "visibility": "direct",
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "1001",
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            ch.send(&SendMessage {
                content: "hello there".into(),
                recipient: "alice@mastodon.social".into(),
                subject: None,
                thread_ts: None,
                cancellation_token: None,
                attachments: vec![],
            })
            .await
            .expect("send succeeded");

            // wiremock's `.expect(1)` enforces exactly one matching request on drop.
        }

        #[tokio::test]
        async fn send_threads_chunks_off_first_post_id() {
            let server = MockServer::start().await;
            // Build a body comfortably over the 500-char status limit so the
            // chunker is forced to emit at least two posts.
            let body = "Mastodon has a 500-character per-status limit, ".repeat(15);
            let body = body.trim().to_string();
            assert!(body.chars().count() > 500, "test fixture too short");
            let body = body.as_str();

            // First chunk: no in_reply_to_id, returns id "first".
            Mock::given(method("POST"))
                .and(path("/api/v1/statuses"))
                .and(body_partial_json(
                    serde_json::json!({"visibility": "direct"}),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "first",
                })))
                .up_to_n_times(1)
                .mount(&server)
                .await;
            // Subsequent chunks: must reference id "first" as in_reply_to_id.
            Mock::given(method("POST"))
                .and(path("/api/v1/statuses"))
                .and(body_partial_json(serde_json::json!({
                    "in_reply_to_id": "first",
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "subsequent",
                })))
                .mount(&server)
                .await;

            let ch = MastodonChannel::new(
                server.uri(),
                "test-token".into(),
                vec!["*".into()],
                false,
                MastodonVisibility::Direct,
                60,
            );
            ch.send(&SendMessage {
                content: body.into(),
                recipient: "alice@mastodon.social".into(),
                subject: None,
                thread_ts: None,
                cancellation_token: None,
                attachments: vec![],
            })
            .await
            .expect("send succeeded");

            // Confirm at least 2 POSTs happened (first + ≥1 continuation).
            let received = server.received_requests().await.expect("server replays");
            assert!(
                received.len() >= 2,
                "expected ≥2 POSTs for chunked body, got {}",
                received.len()
            );

            // Sanity check: every continuation request body carries a
            // mention prefix and the (i/N) marker.
            for req in received.iter().skip(1) {
                let body: Value = serde_json::from_slice(&req.body).expect("json body");
                let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
                assert!(
                    status.starts_with("@alice@mastodon.social ("),
                    "missing chunk header: {status}"
                );
            }
        }

        #[tokio::test]
        async fn send_propagates_thread_ts_as_in_reply_to() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/statuses"))
                .and(body_partial_json(serde_json::json!({
                    "in_reply_to_id": "555",
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "1002",
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            ch.send(&SendMessage {
                content: "thanks for the ping".into(),
                recipient: "alice@mastodon.social".into(),
                subject: None,
                thread_ts: Some("555".into()),
                cancellation_token: None,
                attachments: vec![],
            })
            .await
            .expect("send succeeded");
        }

        #[tokio::test]
        async fn send_surfaces_4xx_as_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/statuses"))
                .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            let err = ch
                .send(&SendMessage {
                    content: "hi".into(),
                    recipient: "alice@mastodon.social".into(),
                    subject: None,
                    thread_ts: None,
                    cancellation_token: None,
                    attachments: vec![],
                })
                .await
                .expect_err("expected 401 to bubble");
            let msg = format!("{err:#}");
            assert!(msg.contains("401"), "missing status in error: {msg}");
        }

        #[tokio::test]
        async fn ensure_identity_caches_verify_credentials() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/accounts/verify_credentials"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "42",
                    "username": "zeroclaw",
                    "acct": "zeroclaw",
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            // First call hits the wire, second must use the cache.
            let first = ch.ensure_identity().await.expect("first call");
            let second = ch.ensure_identity().await.expect("cached call");
            assert_eq!(first.id, "42");
            assert_eq!(second.username, "zeroclaw");
            // wiremock's `.expect(1)` asserts exactly one network call on drop.
        }
    }
}
