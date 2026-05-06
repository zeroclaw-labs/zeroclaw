//! Lemmy channel — private-message polling MVP.
//!
//! Works against any Lemmy-compatible instance (lemmy.world, beehaw.org,
//! self-hosted). v1 focuses on **private messages only**: the simplest and
//! most useful surface for a personal agent. Comment / post listening is
//! deferred to v2.
//!
//! # Auth
//! Two paths. The pre-minted JWT path is checked first; if `jwt` is empty
//! the channel falls back to a username + password login at startup.
//!
//! 1. **Pre-minted JWT**: operator copies a long-lived token from the Lemmy
//!    web UI (browser cookie / admin tools) into `jwt`. Recommended for
//!    production and required for bot accounts with 2FA.
//! 2. **Username + password**: channel calls `POST /api/v3/user/login` once
//!    at listen start to mint a JWT, caches it in memory.
//!
//! Both methods send the JWT as `Authorization: Bearer <jwt>` on every
//! subsequent request.
//!
//! # Inbound
//! Every `poll_interval_secs`, `GET /api/v3/private_message/list?unread_only=true&page=1&limit=20`.
//! For each message: drop self / non-allowed / empty, build a
//! `ChannelMessage` with `reply_target = "pm:{creator_id}"`, then
//! `PUT /api/v3/private_message/mark_as_read` so the next poll doesn't
//! re-deliver it.
//!
//! # Outbound
//! `POST /api/v3/private_message` with `{recipient_id, content}`. Bodies
//! over 10000 chars are split at sentence/word boundaries with `(i/N) `
//! continuation markers. Recipient grammar:
//!
//! * `"pm:{user_id}"` — explicit form (set automatically when replying).
//! * Bare numeric string — same thing, treated as a `pm:` shorthand.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const LEMMY_BODY_SOFT_LIMIT: usize = 10_000;
const LEMMY_PM_PAGE_SIZE: u32 = 20;
const POLL_MIN_SECS: u64 = 5;

pub struct LemmyChannel {
    instance_url: String,
    username: String,
    password: String,
    /// Pre-minted JWT supplied by the operator (takes precedence over
    /// username/password login).
    seed_jwt: String,
    allowed_users: Vec<String>,
    poll_interval: Duration,
    /// Cached auth state — populated by the first login (or seeded from
    /// `seed_jwt`). On 401 we clear and re-login.
    auth: Mutex<Option<AuthState>>,
}

#[derive(Clone)]
struct AuthState {
    jwt: String,
    /// Bot's own user id. Populated by either the login response or the
    /// `getSite` call when only a pre-minted JWT is configured.
    bot_user_id: Option<i64>,
}

#[derive(Serialize)]
struct LoginRequest<'a> {
    username_or_email: &'a str,
    password: &'a str,
}

#[derive(Deserialize)]
struct LoginResponse {
    #[serde(default)]
    jwt: Option<String>,
    /// Lemmy returns errors as `{"error": "incorrect_login"}` rather than
    /// HTTP non-2xx in some versions. Surface that as a typed error.
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct GetSiteResponse {
    #[serde(default)]
    my_user: Option<MyUserSection>,
}

#[derive(Deserialize)]
struct MyUserSection {
    local_user_view: LocalUserView,
}

#[derive(Deserialize)]
struct LocalUserView {
    person: PersonInfo,
}

#[derive(Deserialize, Clone)]
struct PersonInfo {
    id: i64,
}

#[derive(Deserialize, Clone, Debug)]
struct PrivateMessageListResponse {
    #[serde(default)]
    private_messages: Vec<PrivateMessageView>,
}

#[derive(Deserialize, Clone, Debug)]
struct PrivateMessageView {
    private_message: PrivateMessage,
    creator: Person,
}

#[derive(Deserialize, Clone, Debug)]
struct PrivateMessage {
    id: i64,
    #[serde(default)]
    content: String,
    #[serde(default)]
    read: bool,
    #[serde(default)]
    deleted: bool,
    /// ISO-8601 published timestamp.
    #[serde(default)]
    published: String,
}

#[derive(Deserialize, Clone, Debug)]
struct Person {
    id: i64,
    #[serde(default)]
    name: String,
    /// Some Lemmy versions emit `local: bool` to indicate same-instance.
    /// Older versions emit `actor_id` as a URL we'd have to parse for the
    /// instance. We accept both forms via a flexible fallback in
    /// `acct_form()`.
    #[serde(default)]
    local: Option<bool>,
    #[serde(default)]
    actor_id: Option<String>,
}

#[derive(Serialize)]
struct CreatePmRequest<'a> {
    content: &'a str,
    recipient_id: i64,
}

#[derive(Deserialize)]
struct CreatePmResponse {
    #[serde(default)]
    error: Option<String>,
}

#[derive(Serialize)]
struct MarkPmReadRequest {
    private_message_id: i64,
    read: bool,
}

#[derive(Deserialize)]
struct MarkPmReadResponse {
    #[serde(default)]
    error: Option<String>,
}

impl LemmyChannel {
    pub fn new(
        instance_url: String,
        username: String,
        password: String,
        jwt: String,
        allowed_users: Vec<String>,
        poll_interval_secs: u64,
    ) -> Self {
        Self {
            instance_url: normalize_instance_url(&instance_url),
            username,
            password,
            seed_jwt: jwt,
            allowed_users,
            poll_interval: Duration::from_secs(poll_interval_secs.max(POLL_MIN_SECS)),
            auth: Mutex::new(None),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.lemmy")
    }

    fn rest_url(&self, path: &str) -> String {
        format!("{}{}", self.instance_url, path)
    }

    fn current_auth(&self) -> Option<AuthState> {
        self.auth.lock().clone()
    }

    /// Acquire a JWT, preferring the seed JWT when one is configured.
    /// Populates `auth.bot_user_id` lazily via a separate `getSite` call.
    async fn ensure_auth(&self) -> Result<AuthState> {
        if let Some(state) = self.current_auth() {
            return Ok(state);
        }
        let jwt = if !self.seed_jwt.is_empty() {
            self.seed_jwt.clone()
        } else {
            self.login().await?
        };
        let mut state = AuthState {
            jwt: jwt.clone(),
            bot_user_id: None,
        };
        // Resolve the bot's own user id once, used for self-suppression.
        match self.fetch_my_user_id(&jwt).await {
            Ok(Some(id)) => state.bot_user_id = Some(id),
            Ok(None) => {
                tracing::warn!(
                    "Lemmy: getSite returned no my_user; self-suppression disabled until next login"
                );
            }
            Err(e) => {
                tracing::warn!("Lemmy: getSite failed: {e}");
            }
        }
        *self.auth.lock() = Some(state.clone());
        Ok(state)
    }

    async fn login(&self) -> Result<String> {
        if self.username.is_empty() || self.password.is_empty() {
            bail!("Lemmy: jwt is empty and username/password is missing");
        }
        let body = LoginRequest {
            username_or_email: &self.username,
            password: &self.password,
        };
        let resp = self
            .http_client()
            .post(self.rest_url("/api/v3/user/login"))
            .json(&body)
            .send()
            .await
            .context("Lemmy /api/v3/user/login request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Lemmy login returned {status}: {text}");
        }
        let payload: LoginResponse = resp
            .json()
            .await
            .context("Lemmy login returned non-JSON body")?;
        if let Some(err) = payload.error {
            bail!("Lemmy login error: {err}");
        }
        payload
            .jwt
            .filter(|j| !j.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Lemmy login response missing jwt"))
    }

    async fn fetch_my_user_id(&self, jwt: &str) -> Result<Option<i64>> {
        let resp = self
            .http_client()
            .get(self.rest_url("/api/v3/site"))
            .bearer_auth(jwt)
            .send()
            .await
            .context("Lemmy /api/v3/site request failed")?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let payload: GetSiteResponse = resp
            .json()
            .await
            .context("Lemmy getSite returned non-JSON body")?;
        Ok(payload.my_user.map(|m| m.local_user_view.person.id))
    }

    /// Convert a private-message view into a `ChannelMessage`, applying the
    /// self-suppression and allowlist filters.
    fn parse_private_message(
        &self,
        view: &PrivateMessageView,
        bot_user_id: Option<i64>,
    ) -> Option<ChannelMessage> {
        let pm = &view.private_message;
        let creator = &view.creator;

        if pm.read || pm.deleted {
            return None;
        }
        if Some(creator.id) == bot_user_id {
            return None;
        }
        if !is_user_allowed(&creator.acct_form(), &self.allowed_users) {
            tracing::debug!(
                "Lemmy: dropping PM from {} (not in allowed_users)",
                creator.name
            );
            return None;
        }
        let body = pm.content.trim();
        if body.is_empty() {
            return None;
        }

        let timestamp = chrono::DateTime::parse_from_rfc3339(&pm.published)
            .map(|dt| dt.timestamp().cast_unsigned())
            .unwrap_or_else(|_| chrono::Utc::now().timestamp().cast_unsigned());

        Some(ChannelMessage {
            id: format!("lemmy_{}", pm.id),
            sender: creator.acct_form(),
            reply_target: format!("pm:{}", creator.id),
            content: body.to_string(),
            channel: "lemmy".to_string(),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        })
    }

    async fn fetch_unread_pms(&self, jwt: &str) -> Result<Vec<PrivateMessageView>> {
        let resp = self
            .http_client()
            .get(self.rest_url("/api/v3/private_message/list"))
            .bearer_auth(jwt)
            .query(&[
                ("unread_only", "true"),
                ("page", "1"),
                ("limit", &LEMMY_PM_PAGE_SIZE.to_string()),
            ])
            .send()
            .await
            .context("Lemmy /api/v3/private_message/list request failed")?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            *self.auth.lock() = None;
            bail!("Lemmy unauthorized — clearing cached auth and re-logging in");
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Lemmy PM list returned {status}: {body}");
        }
        let payload: PrivateMessageListResponse = resp
            .json()
            .await
            .context("Lemmy PM list returned non-JSON body")?;
        Ok(payload.private_messages)
    }

    async fn mark_pm_read(&self, jwt: &str, pm_id: i64) -> Result<()> {
        let body = MarkPmReadRequest {
            private_message_id: pm_id,
            read: true,
        };
        let resp = self
            .http_client()
            .post(self.rest_url("/api/v3/private_message/mark_as_read"))
            .bearer_auth(jwt)
            .json(&body)
            .send()
            .await
            .context("Lemmy mark_as_read request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Lemmy mark_as_read returned {status}: {text}");
        }
        let payload: MarkPmReadResponse = resp
            .json()
            .await
            .context("Lemmy mark_as_read returned non-JSON body")?;
        if let Some(err) = payload.error {
            bail!("Lemmy mark_as_read error: {err}");
        }
        Ok(())
    }

    async fn post_pm_chunks(
        &self,
        jwt: &str,
        recipient_id: i64,
        chunks: Vec<String>,
    ) -> Result<()> {
        for chunk in chunks {
            let body = CreatePmRequest {
                content: &chunk,
                recipient_id,
            };
            let resp = self
                .http_client()
                .post(self.rest_url("/api/v3/private_message"))
                .bearer_auth(jwt)
                .json(&body)
                .send()
                .await
                .context("Lemmy create-PM request failed")?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                bail!("Lemmy create-PM returned {status}: {text}");
            }
            let payload: CreatePmResponse = resp
                .json()
                .await
                .context("Lemmy create-PM returned non-JSON body")?;
            if let Some(err) = payload.error {
                bail!("Lemmy create-PM error: {err}");
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for LemmyChannel {
    fn name(&self) -> &str {
        "lemmy"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let recipient_id = parse_recipient(&message.recipient)
            .with_context(|| format!("invalid Lemmy recipient: {:?}", message.recipient))?;
        let chunks = chunk_text(&message.content, LEMMY_BODY_SOFT_LIMIT);
        if chunks.is_empty() {
            return Ok(());
        }
        let auth = self.ensure_auth().await?;
        self.post_pm_chunks(&auth.jwt, recipient_id, chunks).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!(
            "Lemmy channel listening (instance={}, poll={}s)",
            self.instance_url,
            self.poll_interval.as_secs()
        );
        loop {
            tokio::time::sleep(self.poll_interval).await;
            let auth = match self.ensure_auth().await {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("Lemmy auth failed: {e}");
                    continue;
                }
            };
            let pms = match self.fetch_unread_pms(&auth.jwt).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Lemmy fetch error: {e}");
                    continue;
                }
            };
            for view in &pms {
                let pm_id = view.private_message.id;
                if let Some(channel_msg) = self.parse_private_message(view, auth.bot_user_id) {
                    if tx.send(channel_msg).await.is_err() {
                        return Ok(());
                    }
                    if let Err(e) = self.mark_pm_read(&auth.jwt, pm_id).await {
                        tracing::warn!("Lemmy mark_as_read({pm_id}) failed: {e}");
                    }
                } else {
                    // Even when we drop a PM (read filter, allowlist miss),
                    // mark it read so it doesn't keep reappearing in the
                    // unread list. Suppress errors — best-effort.
                    let _ = self.mark_pm_read(&auth.jwt, pm_id).await;
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        match self.ensure_auth().await {
            Ok(auth) => self
                .http_client()
                .get(self.rest_url("/api/v3/site"))
                .bearer_auth(&auth.jwt)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false),
            Err(_) => false,
        }
    }
}

impl Person {
    /// Return either the bare username (`"alice"`) for local accounts, or
    /// the qualified `user@instance` form for federated accounts. Falls back
    /// to the bare username when neither field is populated.
    fn acct_form(&self) -> String {
        if self.local == Some(true) {
            return self.name.clone();
        }
        if let Some(actor) = self.actor_id.as_deref()
            && let Some(host) = extract_host(actor)
        {
            return format!("{}@{host}", self.name);
        }
        self.name.clone()
    }
}

fn extract_host(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    after_scheme.split('/').next().filter(|s| !s.is_empty())
}

fn normalize_instance_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

/// Allowlist match. `*` matches anyone. Comparison is case-insensitive.
/// Bare `"alice"` allowlist entries match both bare `"alice"` and
/// instance-qualified `"alice@…"` senders; qualified entries require an
/// exact match on the full string.
pub fn is_user_allowed(acct: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return false;
    }
    if allowlist.iter().any(|u| u == "*") {
        return true;
    }
    let normalized = acct.to_ascii_lowercase();
    let bare = normalized.split('@').next().unwrap_or("").to_string();
    allowlist.iter().any(|entry| {
        let canon = entry.to_ascii_lowercase();
        if canon.contains('@') {
            canon == normalized
        } else {
            canon == bare
        }
    })
}

/// Parse a `SendMessage.recipient` into a numeric `recipient_id`. Accepts
/// `"pm:42"` (canonical) and bare `"42"` (shorthand).
pub fn parse_recipient(recipient: &str) -> Result<i64> {
    let trimmed = recipient.trim();
    if trimmed.is_empty() {
        bail!("empty recipient");
    }
    let id_str = trimmed.strip_prefix("pm:").unwrap_or(trimmed).trim();
    id_str
        .parse::<i64>()
        .with_context(|| format!("expected pm:<id> or numeric id, got {recipient:?}"))
}

/// Split a body into ≤`limit`-character chunks. Single-chunk bodies pass
/// through; multi-chunk bodies receive an `(i/N) ` prefix on each part.
pub fn chunk_text(body: &str, limit: usize) -> Vec<String> {
    let body = body.trim();
    if body.is_empty() {
        return vec![];
    }
    if body.chars().count() <= limit {
        return vec![body.to_string()];
    }
    const MARKER_RESERVE: usize = 8;
    let body_budget = limit.saturating_sub(MARKER_RESERVE).max(1);
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
        return chunks;
    }
    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| format!("({}/{total}) {c}", i + 1))
        .collect()
}

fn pick_split_point(text: &str, char_budget: usize) -> usize {
    let mut budget_idx = text.len();
    for (i, (byte_idx, _)) in text.char_indices().enumerate() {
        if i == char_budget {
            budget_idx = byte_idx;
            break;
        }
    }
    let head = &text[..budget_idx];
    if let Some(idx) = head.rfind(['.', '!', '?', '\n']) {
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

    fn channel_with(allowlist: Vec<String>) -> LemmyChannel {
        LemmyChannel::new(
            "https://lemmy.example".into(),
            "agent-bot".into(),
            "test-pass".into(),
            "".into(),
            allowlist,
            30,
        )
    }

    fn pm_view(
        pm_id: i64,
        creator_id: i64,
        creator_name: &str,
        local: bool,
        actor_id: Option<&str>,
        body: &str,
        read: bool,
    ) -> PrivateMessageView {
        PrivateMessageView {
            private_message: PrivateMessage {
                id: pm_id,
                content: body.into(),
                read,
                deleted: false,
                published: "2026-05-06T12:00:00.000Z".into(),
            },
            creator: Person {
                id: creator_id,
                name: creator_name.into(),
                local: Some(local),
                actor_id: actor_id.map(str::to_string),
            },
        }
    }

    #[test]
    fn normalize_instance_url_adds_https_and_strips_trailing_slash() {
        assert_eq!(normalize_instance_url("lemmy.world"), "https://lemmy.world");
        assert_eq!(
            normalize_instance_url("https://lemmy.world/"),
            "https://lemmy.world"
        );
        assert_eq!(
            normalize_instance_url("  http://localhost:8536  "),
            "http://localhost:8536"
        );
    }

    #[test]
    fn extract_host_handles_full_actor_url() {
        assert_eq!(
            extract_host("https://lemmy.world/u/alice"),
            Some("lemmy.world")
        );
        assert_eq!(
            extract_host("http://localhost:8536/u/dev"),
            Some("localhost:8536")
        );
        assert_eq!(extract_host(""), None);
    }

    #[test]
    fn person_acct_local_uses_bare_name() {
        let p = Person {
            id: 1,
            name: "alice".into(),
            local: Some(true),
            actor_id: Some("https://lemmy.example/u/alice".into()),
        };
        assert_eq!(p.acct_form(), "alice");
    }

    #[test]
    fn person_acct_remote_uses_qualified_form() {
        let p = Person {
            id: 1,
            name: "alice".into(),
            local: Some(false),
            actor_id: Some("https://lemmy.world/u/alice".into()),
        };
        assert_eq!(p.acct_form(), "alice@lemmy.world");
    }

    #[test]
    fn person_acct_falls_back_to_name_when_actor_missing() {
        let p = Person {
            id: 1,
            name: "alice".into(),
            local: None,
            actor_id: None,
        };
        assert_eq!(p.acct_form(), "alice");
    }

    #[test]
    fn allowlist_empty_denies_everyone() {
        assert!(!is_user_allowed("alice", &[]));
    }

    #[test]
    fn allowlist_wildcard_allows_anyone() {
        let allow = vec!["*".into()];
        assert!(is_user_allowed("alice", &allow));
        assert!(is_user_allowed("bob@elsewhere.tld", &allow));
    }

    #[test]
    fn allowlist_bare_entry_matches_both_forms() {
        let allow = vec!["alice".into()];
        assert!(is_user_allowed("alice", &allow));
        assert!(is_user_allowed("Alice@lemmy.world", &allow));
    }

    #[test]
    fn allowlist_qualified_entry_requires_exact_match() {
        let allow = vec!["alice@lemmy.world".into()];
        assert!(is_user_allowed("alice@lemmy.world", &allow));
        assert!(!is_user_allowed("alice", &allow));
        assert!(!is_user_allowed("alice@beehaw.org", &allow));
    }

    #[test]
    fn parse_recipient_pm_form() {
        assert_eq!(parse_recipient("pm:42").unwrap(), 42);
        assert_eq!(parse_recipient("  pm:1234  ").unwrap(), 1234);
    }

    #[test]
    fn parse_recipient_bare_numeric() {
        assert_eq!(parse_recipient("42").unwrap(), 42);
    }

    #[test]
    fn parse_recipient_rejects_non_numeric() {
        assert!(parse_recipient("alice").is_err());
        assert!(parse_recipient("pm:abc").is_err());
        assert!(parse_recipient("").is_err());
    }

    #[test]
    fn parse_pm_drops_self() {
        let ch = channel_with(vec!["*".into()]);
        let view = pm_view(1, 5, "self-bot", true, None, "echo", false);
        assert!(ch.parse_private_message(&view, Some(5)).is_none());
    }

    #[test]
    fn parse_pm_drops_already_read() {
        let ch = channel_with(vec!["*".into()]);
        let view = pm_view(1, 6, "alice", true, None, "old", true);
        assert!(ch.parse_private_message(&view, None).is_none());
    }

    #[test]
    fn parse_pm_drops_outside_allowlist() {
        let ch = channel_with(vec!["alice".into()]);
        let view = pm_view(1, 6, "stranger", true, None, "hi", false);
        assert!(ch.parse_private_message(&view, None).is_none());
    }

    #[test]
    fn parse_pm_drops_empty_body() {
        let ch = channel_with(vec!["*".into()]);
        let view = pm_view(1, 6, "alice", true, None, "   ", false);
        assert!(ch.parse_private_message(&view, None).is_none());
    }

    #[test]
    fn parse_pm_drops_deleted() {
        let ch = channel_with(vec!["*".into()]);
        let mut view = pm_view(1, 6, "alice", true, None, "hi", false);
        view.private_message.deleted = true;
        assert!(ch.parse_private_message(&view, None).is_none());
    }

    #[test]
    fn parse_pm_accepts_valid_local() {
        let ch = channel_with(vec!["alice".into()]);
        let view = pm_view(7, 6, "alice", true, None, "ping", false);
        let msg = ch.parse_private_message(&view, None).expect("expected msg");
        assert_eq!(msg.id, "lemmy_7");
        assert_eq!(msg.sender, "alice");
        assert_eq!(msg.reply_target, "pm:6");
        assert_eq!(msg.content, "ping");
        assert_eq!(msg.channel, "lemmy");
    }

    #[test]
    fn parse_pm_accepts_valid_remote_with_qualified_sender() {
        let ch = channel_with(vec!["alice@lemmy.world".into()]);
        let view = pm_view(
            8,
            9,
            "alice",
            false,
            Some("https://lemmy.world/u/alice"),
            "federated ping",
            false,
        );
        let msg = ch.parse_private_message(&view, None).expect("expected msg");
        assert_eq!(msg.sender, "alice@lemmy.world");
    }

    #[test]
    fn chunk_text_short_passes_through() {
        let chunks = chunk_text("hi there", LEMMY_BODY_SOFT_LIMIT);
        assert_eq!(chunks, vec!["hi there"]);
    }

    #[test]
    fn chunk_text_long_is_split_with_marker() {
        let body = "alpha beta gamma. ".repeat(800);
        let chunks = chunk_text(&body, 200);
        assert!(chunks.len() >= 2);
        for (i, c) in chunks.iter().enumerate() {
            assert!(c.starts_with(&format!("({}/", i + 1)));
            assert!(c.chars().count() <= 200);
        }
    }

    #[test]
    fn chunk_text_empty_returns_no_chunks() {
        let chunks = chunk_text("   ", LEMMY_BODY_SOFT_LIMIT);
        assert!(chunks.is_empty());
    }

    mod http_tests {
        use super::*;
        use serde_json::json;
        use wiremock::matchers::{
            body_partial_json, header, header_exists, method, path, query_param,
        };
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn channel_for(server_uri: &str, jwt: Option<&str>) -> LemmyChannel {
            LemmyChannel::new(
                server_uri.to_string(),
                "agent-bot".into(),
                "test-pass".into(),
                jwt.unwrap_or("").into(),
                vec!["*".into()],
                30,
            )
        }

        #[tokio::test]
        async fn login_mints_jwt_and_caches_it() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v3/user/login"))
                .and(body_partial_json(json!({
                    "username_or_email": "agent-bot",
                    "password": "test-pass"
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "jwt": "JWT-1"
                })))
                .expect(1)
                .mount(&server)
                .await;
            // /api/v3/site is called once during ensure_auth to populate
            // the bot user id; return a fixture so that path doesn't fail.
            Mock::given(method("GET"))
                .and(path("/api/v3/site"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "my_user": {
                        "local_user_view": {
                            "person": {"id": 999}
                        }
                    }
                })))
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri(), None);
            let auth = ch.ensure_auth().await.expect("auth ok");
            assert_eq!(auth.jwt, "JWT-1");
            assert_eq!(auth.bot_user_id, Some(999));
            // Second call must not re-mint.
            let _ = ch.ensure_auth().await.expect("cached");
        }

        #[tokio::test]
        async fn seed_jwt_skips_login() {
            let server = MockServer::start().await;
            // No login mock — if the channel calls login, the test fails.
            Mock::given(method("GET"))
                .and(path("/api/v3/site"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "my_user": {
                        "local_user_view": {
                            "person": {"id": 11}
                        }
                    }
                })))
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri(), Some("seed-jwt"));
            let auth = ch.ensure_auth().await.expect("auth ok");
            assert_eq!(auth.jwt, "seed-jwt");
            assert_eq!(auth.bot_user_id, Some(11));
        }

        #[tokio::test]
        async fn send_posts_pm_with_bearer_auth() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v3/site"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "my_user": {"local_user_view": {"person": {"id": 1}}}
                })))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/api/v3/private_message"))
                .and(header_exists("authorization"))
                .and(body_partial_json(json!({
                    "content": "hello",
                    "recipient_id": 42
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "private_message_view": {"private_message": {"id": 1000}}
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri(), Some("seed-jwt"));
            ch.send(&SendMessage {
                content: "hello".into(),
                recipient: "pm:42".into(),
                subject: None,
                thread_ts: None,
                cancellation_token: None,
                attachments: vec![],
            })
            .await
            .expect("send succeeded");
        }

        #[tokio::test]
        async fn send_surfaces_4xx_as_error() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v3/site"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "my_user": {"local_user_view": {"person": {"id": 1}}}
                })))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/api/v3/private_message"))
                .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri(), Some("seed-jwt"));
            let err = ch
                .send(&SendMessage {
                    content: "hi".into(),
                    recipient: "pm:42".into(),
                    subject: None,
                    thread_ts: None,
                    cancellation_token: None,
                    attachments: vec![],
                })
                .await
                .expect_err("expected 401");
            assert!(format!("{err:#}").contains("401"));
        }

        #[tokio::test]
        async fn fetch_unread_pms_calls_with_query_params() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v3/private_message/list"))
                .and(query_param("unread_only", "true"))
                .and(query_param("page", "1"))
                .and(header("authorization", "Bearer seed-jwt"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "private_messages": [{
                        "private_message": {
                            "id": 7,
                            "content": "hi bot",
                            "read": false,
                            "deleted": false,
                            "published": "2026-05-06T12:00:00.000Z"
                        },
                        "creator": {
                            "id": 6,
                            "name": "alice",
                            "local": true
                        }
                    }]
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri(), Some("seed-jwt"));
            let pms = ch.fetch_unread_pms("seed-jwt").await.expect("ok");
            assert_eq!(pms.len(), 1);
            assert_eq!(pms[0].private_message.id, 7);
        }

        #[tokio::test]
        async fn fetch_unread_pms_clears_auth_on_401() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v3/private_message/list"))
                .respond_with(ResponseTemplate::new(401).set_body_string("token expired"))
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri(), Some("seed-jwt"));
            // Seed the auth cache so we can observe the clear.
            *ch.auth.lock() = Some(AuthState {
                jwt: "seed-jwt".into(),
                bot_user_id: Some(1),
            });
            let err = ch
                .fetch_unread_pms("seed-jwt")
                .await
                .expect_err("expected 401 err");
            assert!(format!("{err:#}").contains("unauthorized"));
            assert!(ch.auth.lock().is_none(), "auth should be cleared on 401");
        }

        #[tokio::test]
        async fn mark_pm_read_posts_id_and_read_flag() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v3/private_message/mark_as_read"))
                .and(header_exists("authorization"))
                .and(body_partial_json(json!({
                    "private_message_id": 7,
                    "read": true
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "private_message_view": {"private_message": {"id": 7, "read": true}}
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri(), Some("seed-jwt"));
            ch.mark_pm_read("seed-jwt", 7).await.expect("ok");
        }
    }
}
