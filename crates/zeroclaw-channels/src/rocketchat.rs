//! Rocket.Chat channel — REST polling MVP.
//!
//! Works against any Rocket.Chat-compatible server (rocket.chat-cloud or any
//! self-hosted instance). Authenticates via a Personal Access Token, polls
//! each configured `room_id` for new messages, and replies through
//! `chat.postMessage`.
//!
//! # Auth
//! Personal Access Token. Rocket.Chat's PAT model requires both an
//! `X-Auth-Token` header (the token itself) and an `X-User-Id` header (the
//! bot account's `_id`). The operator copies both from the PAT creation
//! dialog in the Rocket.Chat web UI.
//!
//! # Inbound
//! For each `room_id` in config, polls
//! `GET {server}/api/v1/chat.syncMessages?roomId={rid}&lastUpdate={iso8601}`
//! every `poll_interval_secs`. The cursor is the most recent `_updatedAt`
//! seen on a delivered message. Filters: drop messages from the bot's own
//! `user_id`, drop senders not on `allowed_users`, drop empty/non-text bodies.
//!
//! # Outbound
//! `POST {server}/api/v1/chat.postMessage` with a JSON body of `roomId` plus
//! `text`. The recipient encoded in `SendMessage` is treated as the
//! Rocket.Chat room id directly. Bodies over 4kB are split at sentence/word
//! boundaries with `(i/N) ` continuation markers — Rocket.Chat does not
//! enforce a per-message character limit, but very long messages render
//! awkwardly in the UI.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const ROCKETCHAT_BODY_SOFT_LIMIT: usize = 4000;
const POLL_MIN_SECS: u64 = 2;
const SYNC_INITIAL_LOOKBACK_SECS: i64 = 60;

pub struct RocketChatChannel {
    server_url: String,
    auth_token: String,
    user_id: String,
    allowed_users: Vec<String>,
    room_ids: Vec<String>,
    poll_interval: Duration,
    /// Per-room cursor of the most recent `_updatedAt` we've already
    /// dispatched. Initialised lazily on first poll.
    cursors: Mutex<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct ChatPostResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct SyncResult {
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    result: Option<SyncPayload>,
}

#[derive(Deserialize)]
struct SyncPayload {
    #[serde(default)]
    updated: Vec<RcMessage>,
    // `deleted` is intentionally ignored — agent ingestion is append-only.
}

#[derive(Deserialize, Clone)]
struct RcMessage {
    #[serde(rename = "_id")]
    id: String,
    #[serde(rename = "rid")]
    room_id: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    u: Option<RcUser>,
    /// ISO-8601 timestamp Rocket.Chat assigns/updates on every revision.
    /// Rocket.Chat uses `{"$date": "..."}` mongoextjson on some endpoints
    /// and a plain ISO string on others; tolerate both.
    #[serde(
        rename = "_updatedAt",
        default,
        deserialize_with = "deserialize_rc_timestamp"
    )]
    updated_at: Option<String>,
    /// Original message timestamp (creation). Used as `ChannelMessage`
    /// timestamp and as a tiebreaker on the sync cursor.
    #[serde(default, deserialize_with = "deserialize_rc_timestamp")]
    ts: Option<String>,
    /// Bot/system messages set this to suppress agent reactions.
    #[serde(default)]
    bot: Option<serde_json::Value>,
    /// Edits set this; treat the message as updated rather than new.
    #[serde(
        default,
        rename = "editedAt",
        deserialize_with = "deserialize_rc_timestamp"
    )]
    edited_at: Option<String>,
}

#[derive(Deserialize, Clone)]
struct RcUser {
    #[serde(rename = "_id", default)]
    id: String,
    #[serde(default)]
    username: String,
}

#[derive(Serialize)]
struct PostMessageRequest<'a> {
    #[serde(rename = "roomId")]
    room_id: &'a str,
    text: &'a str,
}

impl RocketChatChannel {
    pub fn new(
        server_url: String,
        auth_token: String,
        user_id: String,
        allowed_users: Vec<String>,
        room_ids: Vec<String>,
        poll_interval_secs: u64,
    ) -> Self {
        Self {
            server_url: normalize_server_url(&server_url),
            auth_token,
            user_id,
            allowed_users,
            room_ids,
            poll_interval: Duration::from_secs(poll_interval_secs.max(POLL_MIN_SECS)),
            cursors: Mutex::new(HashMap::new()),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.rocketchat")
    }

    fn rest_url(&self, path: &str) -> String {
        format!("{}{}", self.server_url, path)
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("X-Auth-Token", &self.auth_token)
            .header("X-User-Id", &self.user_id)
    }

    /// Convert a Rocket.Chat message into a `ChannelMessage`, applying the
    /// self-suppression and allowlist filters. Returns `None` when the
    /// message should be dropped.
    fn parse_message(&self, raw: &RcMessage) -> Option<ChannelMessage> {
        let user = raw.u.as_ref()?;

        // Skip the bot's own posts (Rocket.Chat returns them via syncMessages).
        if user.id == self.user_id {
            return None;
        }

        // Skip system / bot-flagged messages — those are not user prompts.
        if raw.bot.is_some() {
            return None;
        }

        if !is_user_allowed(&user.username, &self.allowed_users) {
            tracing::debug!(
                "RocketChat: dropping message from {} (not in allowed_users)",
                user.username
            );
            return None;
        }

        let body = raw.msg.trim();
        if body.is_empty() {
            return None;
        }

        // Skip edits — agent ingestion is append-only and re-running the
        // agent on every edit would double-spend.
        if raw.edited_at.is_some() {
            return None;
        }

        let timestamp = raw
            .ts
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp().cast_unsigned())
            .unwrap_or_else(|| chrono::Utc::now().timestamp().cast_unsigned());

        Some(ChannelMessage {
            id: format!("rocketchat_{}", raw.id),
            sender: user.username.clone(),
            // Reply target is the room id — `chat.postMessage` accepts that
            // for DMs, channels, and private groups uniformly.
            reply_target: raw.room_id.clone(),
            content: body.to_string(),
            channel: "rocketchat".to_string(),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        })
    }

    async fn fetch_room(&self, room_id: &str) -> Result<Vec<RcMessage>> {
        let last_update = {
            let cursors = self.cursors.lock();
            cursors.get(room_id).cloned()
        };
        let last_update = last_update.unwrap_or_else(|| {
            (chrono::Utc::now() - chrono::Duration::seconds(SYNC_INITIAL_LOOKBACK_SECS))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        });

        let req = self
            .http_client()
            .get(self.rest_url("/api/v1/chat.syncMessages"))
            .query(&[("roomId", room_id), ("lastUpdate", last_update.as_str())]);
        let resp = self
            .apply_auth(req)
            .send()
            .await
            .context("Rocket.Chat chat.syncMessages request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Rocket.Chat sync returned {status}: {body}");
        }

        let payload: SyncResult = resp
            .json()
            .await
            .context("Rocket.Chat sync returned non-JSON body")?;
        if !payload.success {
            bail!(
                "Rocket.Chat sync error: {}",
                payload.error.unwrap_or_else(|| "unknown".into())
            );
        }
        Ok(payload.result.map(|p| p.updated).unwrap_or_default())
    }

    /// Update the per-room cursor to the latest `_updatedAt` we just
    /// observed. Pure helper so it's exercised by unit tests.
    fn advance_cursor(&self, room_id: &str, batch: &[RcMessage]) {
        let Some(latest) = batch.iter().filter_map(|m| m.updated_at.clone()).max() else {
            return;
        };
        let mut cursors = self.cursors.lock();
        cursors.insert(room_id.to_string(), latest);
    }

    async fn post_chunks(&self, room_id: &str, chunks: Vec<String>) -> Result<()> {
        for chunk in chunks {
            let body = PostMessageRequest {
                room_id,
                text: &chunk,
            };
            let req = self
                .http_client()
                .post(self.rest_url("/api/v1/chat.postMessage"))
                .json(&body);
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .context("Rocket.Chat chat.postMessage request failed")?;
            let status = resp.status();
            if !status.is_success() {
                let body_text = resp.text().await.unwrap_or_default();
                bail!("Rocket.Chat post returned {status}: {body_text}");
            }
            let posted: ChatPostResponse = resp
                .json()
                .await
                .context("Rocket.Chat post returned non-JSON body")?;
            if !posted.success {
                bail!(
                    "Rocket.Chat post error: {}",
                    posted.error.unwrap_or_else(|| "unknown".into())
                );
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for RocketChatChannel {
    fn name(&self) -> &str {
        "rocketchat"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let room_id = message.recipient.trim();
        if room_id.is_empty() {
            bail!("Rocket.Chat send: empty recipient (expected room id)");
        }
        let chunks = chunk_text(&message.content, ROCKETCHAT_BODY_SOFT_LIMIT);
        if chunks.is_empty() {
            return Ok(());
        }
        self.post_chunks(room_id, chunks).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        if self.room_ids.is_empty() {
            tracing::warn!("RocketChat: no room_ids configured — listener has nothing to poll");
        }
        tracing::info!(
            "RocketChat channel listening on {} room(s) every {}s",
            self.room_ids.len(),
            self.poll_interval.as_secs()
        );
        loop {
            tokio::time::sleep(self.poll_interval).await;
            for room_id in &self.room_ids {
                let batch = match self.fetch_room(room_id).await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("RocketChat poll error for room {room_id}: {e}");
                        continue;
                    }
                };
                if batch.is_empty() {
                    continue;
                }
                self.advance_cursor(room_id, &batch);
                for raw in &batch {
                    if let Some(msg) = self.parse_message(raw)
                        && tx.send(msg).await.is_err()
                    {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let req = self.http_client().get(self.rest_url("/api/v1/me"));
        self.apply_auth(req)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

fn normalize_server_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

/// Allowlist match. `*` matches anyone. Comparison is case-insensitive and
/// strips a leading `@` so an operator who lists `"@alice"` still matches the
/// raw `"alice"` from the API.
pub fn is_user_allowed(username: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return false;
    }
    if allowlist.iter().any(|u| u == "*") {
        return true;
    }
    let normalized = username.trim_start_matches('@').to_ascii_lowercase();
    allowlist.iter().any(|entry| {
        let canon = entry.trim_start_matches('@').to_ascii_lowercase();
        canon == normalized
    })
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

/// Rocket.Chat encodes timestamps either as plain ISO-8601 strings or as
/// `{"$date": "<iso>"}` objects (mongoextjson). Accept either. Returns
/// `Ok(None)` when the field is missing or null.
fn deserialize_rc_timestamp<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(de)?;
    Ok(match v {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s),
        serde_json::Value::Object(map) => map.get("$date").and_then(|d| {
            if let Some(s) = d.as_str() {
                Some(s.to_string())
            } else {
                d.as_i64().map(|ms| {
                    chrono::DateTime::from_timestamp_millis(ms)
                        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                        .unwrap_or_default()
                })
            }
        }),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_with(allowlist: Vec<String>, rooms: Vec<String>) -> RocketChatChannel {
        RocketChatChannel::new(
            "https://chat.example".into(),
            "test-token".into(),
            "BOTUID".into(),
            allowlist,
            rooms,
            10,
        )
    }

    fn raw_msg(id: &str, room: &str, sender_id: &str, sender: &str, body: &str) -> RcMessage {
        RcMessage {
            id: id.into(),
            room_id: room.into(),
            msg: body.into(),
            u: Some(RcUser {
                id: sender_id.into(),
                username: sender.into(),
            }),
            updated_at: Some("2026-05-06T12:00:00.500Z".into()),
            ts: Some("2026-05-06T12:00:00.000Z".into()),
            bot: None,
            edited_at: None,
        }
    }

    #[test]
    fn normalize_server_url_adds_https_and_strips_trailing_slash() {
        assert_eq!(
            normalize_server_url("chat.example.com"),
            "https://chat.example.com"
        );
        assert_eq!(
            normalize_server_url("https://chat.example.com/"),
            "https://chat.example.com"
        );
        assert_eq!(
            normalize_server_url("  http://localhost:3000  "),
            "http://localhost:3000"
        );
    }

    #[test]
    fn allowlist_empty_denies_everyone() {
        assert!(!is_user_allowed("alice", &[]));
    }

    #[test]
    fn allowlist_wildcard_allows_anyone() {
        let allow = vec!["*".into()];
        assert!(is_user_allowed("alice", &allow));
        assert!(is_user_allowed("bob", &allow));
    }

    #[test]
    fn allowlist_matches_case_insensitive_with_at_prefix_stripped() {
        let allow = vec!["@Alice".into()];
        assert!(is_user_allowed("alice", &allow));
        assert!(is_user_allowed("@ALICE", &allow));
        assert!(!is_user_allowed("bob", &allow));
    }

    #[test]
    fn parse_message_drops_bot_self() {
        let ch = channel_with(vec!["*".into()], vec!["RID1".into()]);
        // sender id == bot user id → drop
        let raw = raw_msg("M1", "RID1", "BOTUID", "zeroclaw-bot", "echo");
        assert!(ch.parse_message(&raw).is_none());
    }

    #[test]
    fn parse_message_drops_outside_allowlist() {
        let ch = channel_with(vec!["alice".into()], vec!["RID1".into()]);
        let raw = raw_msg("M1", "RID1", "U2", "stranger", "hello");
        assert!(ch.parse_message(&raw).is_none());
    }

    #[test]
    fn parse_message_drops_empty_body() {
        let ch = channel_with(vec!["*".into()], vec!["RID1".into()]);
        let raw = raw_msg("M1", "RID1", "U2", "alice", "   ");
        assert!(ch.parse_message(&raw).is_none());
    }

    #[test]
    fn parse_message_drops_bot_flagged() {
        let ch = channel_with(vec!["*".into()], vec!["RID1".into()]);
        let mut raw = raw_msg("M1", "RID1", "U2", "alice", "automated");
        raw.bot = Some(serde_json::json!({"i": "rocketcat"}));
        assert!(ch.parse_message(&raw).is_none());
    }

    #[test]
    fn parse_message_drops_edits() {
        let ch = channel_with(vec!["*".into()], vec!["RID1".into()]);
        let mut raw = raw_msg("M1", "RID1", "U2", "alice", "hi (fixed)");
        raw.edited_at = Some("2026-05-06T12:00:01.000Z".into());
        assert!(ch.parse_message(&raw).is_none());
    }

    #[test]
    fn parse_message_accepts_allowed_user() {
        let ch = channel_with(vec!["alice".into()], vec!["RID1".into()]);
        let raw = raw_msg("M1", "RID1", "U2", "alice", "hello @bot");
        let msg = ch.parse_message(&raw).expect("expected message");
        assert_eq!(msg.sender, "alice");
        assert_eq!(msg.reply_target, "RID1");
        assert_eq!(msg.content, "hello @bot");
        assert_eq!(msg.channel, "rocketchat");
        assert_eq!(msg.id, "rocketchat_M1");
    }

    #[test]
    fn advance_cursor_picks_max_updated_at() {
        let ch = channel_with(vec!["*".into()], vec!["RID1".into()]);
        let mut a = raw_msg("Ma", "RID1", "U2", "alice", "first");
        a.updated_at = Some("2026-05-06T12:00:01.000Z".into());
        let mut b = raw_msg("Mb", "RID1", "U2", "alice", "second");
        b.updated_at = Some("2026-05-06T12:00:05.500Z".into());
        let mut c = raw_msg("Mc", "RID1", "U2", "alice", "third");
        c.updated_at = Some("2026-05-06T12:00:03.250Z".into());
        ch.advance_cursor("RID1", &[a, b, c]);
        let cursors = ch.cursors.lock();
        assert_eq!(
            cursors.get("RID1").cloned().unwrap(),
            "2026-05-06T12:00:05.500Z"
        );
    }

    #[test]
    fn advance_cursor_noop_when_batch_has_no_updated_at() {
        let ch = channel_with(vec!["*".into()], vec!["RID1".into()]);
        let mut a = raw_msg("Ma", "RID1", "U2", "alice", "first");
        a.updated_at = None;
        ch.advance_cursor("RID1", &[a]);
        let cursors = ch.cursors.lock();
        assert!(cursors.get("RID1").is_none());
    }

    #[test]
    fn chunk_text_short_passes_through() {
        let chunks = chunk_text("hi there", 4000);
        assert_eq!(chunks, vec!["hi there"]);
    }

    #[test]
    fn chunk_text_long_is_split_with_marker() {
        let body = "alpha beta gamma. ".repeat(120);
        let chunks = chunk_text(&body, 100);
        assert!(chunks.len() >= 2, "expected ≥2 chunks");
        for (i, c) in chunks.iter().enumerate() {
            assert!(c.starts_with(&format!("({}/", i + 1)));
            assert!(c.chars().count() <= 100);
        }
    }

    #[test]
    fn chunk_text_empty_returns_no_chunks() {
        let chunks = chunk_text("   ", 4000);
        assert!(chunks.is_empty());
    }

    #[test]
    fn deserialize_rc_timestamp_accepts_string() {
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(deserialize_with = "deserialize_rc_timestamp")]
            ts: Option<String>,
        }
        let v: Wrap = serde_json::from_str(r#"{"ts":"2026-05-06T12:00:00.000Z"}"#).unwrap();
        assert_eq!(v.ts.as_deref(), Some("2026-05-06T12:00:00.000Z"));
    }

    #[test]
    fn deserialize_rc_timestamp_accepts_dollar_date_string() {
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(deserialize_with = "deserialize_rc_timestamp")]
            ts: Option<String>,
        }
        let v: Wrap =
            serde_json::from_str(r#"{"ts":{"$date":"2026-05-06T12:00:00.000Z"}}"#).unwrap();
        assert_eq!(v.ts.as_deref(), Some("2026-05-06T12:00:00.000Z"));
    }

    #[test]
    fn deserialize_rc_timestamp_accepts_dollar_date_millis() {
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(deserialize_with = "deserialize_rc_timestamp")]
            ts: Option<String>,
        }
        // 1746528000000 = 2025-05-06T12:00:00.000Z
        let v: Wrap = serde_json::from_str(r#"{"ts":{"$date":1746528000000}}"#).unwrap();
        assert!(v.ts.unwrap().starts_with("2025-05-06"));
    }

    mod http_tests {
        use super::*;
        use serde_json::json;
        use wiremock::matchers::{body_partial_json, header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn channel_for(server_uri: &str) -> RocketChatChannel {
            RocketChatChannel::new(
                server_uri.to_string(),
                "test-token".into(),
                "BOTUID".into(),
                vec!["*".into()],
                vec!["RID1".into()],
                10,
            )
        }

        #[tokio::test]
        async fn send_posts_with_auth_headers_and_room_id() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/chat.postMessage"))
                .and(header("X-Auth-Token", "test-token"))
                .and(header("X-User-Id", "BOTUID"))
                .and(body_partial_json(
                    json!({"roomId": "RID1", "text": "hello"}),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "success": true,
                    "message": { "_id": "M999" }
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            ch.send(&SendMessage {
                content: "hello".into(),
                recipient: "RID1".into(),
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
            Mock::given(method("POST"))
                .and(path("/api/v1/chat.postMessage"))
                .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            let err = ch
                .send(&SendMessage {
                    content: "hi".into(),
                    recipient: "RID1".into(),
                    subject: None,
                    thread_ts: None,
                    cancellation_token: None,
                    attachments: vec![],
                })
                .await
                .expect_err("expected error");
            assert!(format!("{err:#}").contains("401"));
        }

        #[tokio::test]
        async fn send_surfaces_success_false_payload_as_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/chat.postMessage"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "success": false,
                    "error": "channel-not-found"
                })))
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            let err = ch
                .send(&SendMessage {
                    content: "hi".into(),
                    recipient: "RID1".into(),
                    subject: None,
                    thread_ts: None,
                    cancellation_token: None,
                    attachments: vec![],
                })
                .await
                .expect_err("expected error");
            assert!(format!("{err:#}").contains("channel-not-found"));
        }

        #[tokio::test]
        async fn fetch_room_calls_sync_with_cursor_query() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/chat.syncMessages"))
                .and(header("X-Auth-Token", "test-token"))
                .and(header("X-User-Id", "BOTUID"))
                .and(query_param("roomId", "RID1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "success": true,
                    "result": {
                        "updated": [{
                            "_id": "M1",
                            "rid": "RID1",
                            "msg": "hello bot",
                            "u": {"_id": "U2", "username": "alice"},
                            "_updatedAt": "2026-05-06T12:00:00.500Z",
                            "ts": "2026-05-06T12:00:00.000Z"
                        }]
                    }
                })))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            let batch = ch.fetch_room("RID1").await.expect("fetch ok");
            assert_eq!(batch.len(), 1);
            assert_eq!(batch[0].id, "M1");
            assert_eq!(batch[0].msg, "hello bot");

            // Cursor advance against the returned batch.
            ch.advance_cursor("RID1", &batch);
            assert_eq!(
                ch.cursors.lock().get("RID1").cloned().unwrap(),
                "2026-05-06T12:00:00.500Z"
            );
        }

        #[tokio::test]
        async fn health_check_uses_me_endpoint() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/me"))
                .and(header("X-Auth-Token", "test-token"))
                .and(header("X-User-Id", "BOTUID"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"_id": "BOTUID"})))
                .expect(1)
                .mount(&server)
                .await;

            let ch = channel_for(&server.uri());
            assert!(ch.health_check().await);
        }
    }
}
